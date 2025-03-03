//! Helper functionality for transport.

use crate::metrics::SendQueueMetrics;
use crate::types::{SendQueue, SendQueueReader};
use async_trait::async_trait;
use ic_base_types::NodeId;
use ic_interfaces_transport::{TransportChannelId, TransportPayload};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{channel, error::TrySendError, Receiver, Sender};
use tokio::time::Duration;
use tokio::time::{timeout_at, Instant};

// High level design of the send queue flow:
// - tokio::mpsc::channel is used as the send queue. This acts as a single
//   producer single consumer queue
// - Producer: Transport client threads calling transport.send(). They are
//   serialized by the SendQueueImpl.channel_ends mutex to write to the send end
//   of the channel
// - Consumer: the per-connection write task. It takes ownership of the receive
//   end and exclusively owns it
// - Clients like P2P need to clear the queue periodically. ReceiveEndContainer
//   is a light weight wrapper that is shared between the SendQueueImpl and the
//   write task. It is a low contention mechanism to update the write task's
//   receive end, when the channel is recreated
//
//   queue.clear() results in ReceiveEndContainer.update(), to fill in the new
//   receiver. The write task periodically calls ReceiveEndContainer.take()
//   to take ownership of the updated end. Since update() is an infrequent
//   operation, take() should have minimal contention

type SendEnd = Sender<(Instant, TransportPayload)>;
type ReceiveEnd = Receiver<(Instant, TransportPayload)>;

// Since there is no try_recv(), this is the max duration after the first
// dequeue to batch for
/// Maximal time to wait for batching
const MAX_BATCHING_DURATION_MSEC: u64 = 20;

/// Guarded receive end
struct ReceiveEndContainer {
    state: Mutex<Option<ReceiveEnd>>,
}

impl ReceiveEndContainer {
    /// Wraps a given receive end and returns it
    fn new(receive_end: ReceiveEnd) -> Self {
        Self {
            state: Mutex::new(Some(receive_end)),
        }
    }

    /// Set the receive_end state for use by reader.
    /// Returns `None` if the receive_end was successfully set.
    /// Returns `Some(receive_end)` if a state was already present.
    fn try_update(&self, receive_end: ReceiveEnd) -> Result<(), ReceiveEnd> {
        let mut state = self.state.lock().unwrap();
        if state.is_some() {
            // Continue using the current channel, so that the send requests
            // queued so far(before the connection was established) are not
            // dropped
            Err(receive_end)
        } else {
            // Writer task took ownership of the last created channel and exited,
            // accept the new channel
            *state = Some(receive_end);
            Ok(())
        }
    }

    /// Updates the receive end
    fn update(&self, receive_end: ReceiveEnd) {
        let mut state = self.state.lock().unwrap();
        *state = Some(receive_end);
    }

    /// Takes out the currently active receive end (if any)
    fn take(&self) -> Option<ReceiveEnd> {
        let mut state = self.state.lock().unwrap();
        state.take()
    }
}

/// Transport client -> scheduler adapter.
pub(crate) struct SendQueueImpl {
    /// Peer label, string for use as the value for a metric label
    peer_label: String,

    /// Channel id string for use as the value for a metric label
    channel_id: String,

    /// Size of queue
    queue_size: usize,

    // Both the send and receive ends should be updated together.
    send_end: SendEnd,
    receive_end: Arc<ReceiveEndContainer>,

    /// Metrics
    metrics: SendQueueMetrics,
}

/// Implementation for the send queue
impl SendQueueImpl {
    /// Initializes and returns a send queue
    pub(crate) fn new(
        peer_label: String,
        channel_id: TransportChannelId,
        queue_size: usize,
        metrics: SendQueueMetrics,
    ) -> Self {
        let (send_end, receive_end) = channel(queue_size);
        let receieve_end_wrapper = ReceiveEndContainer::new(receive_end);
        Self {
            peer_label,
            channel_id: channel_id.to_string(),
            queue_size,
            send_end,
            receive_end: Arc::new(receieve_end_wrapper),
            metrics,
        }
    }
}

#[async_trait]
impl SendQueue for SendQueueImpl {
    fn get_reader(&mut self) -> Box<dyn SendQueueReader + Send + Sync> {
        let (send_end, receive_end) = channel(self.queue_size);
        if self.receive_end.try_update(receive_end).is_ok() {
            // Receive end was updated, so update send end as well.
            self.send_end = send_end;
        }

        let reader = SendQueueReaderImpl {
            peer_label: self.peer_label.clone(),
            channel_id: self.channel_id.clone(),
            receive_end_container: self.receive_end.clone(),
            cur_receive_end: None,
            metrics: self.metrics.clone(),
        };
        Box::new(reader)
    }

    fn enqueue(&self, message: TransportPayload) -> Option<TransportPayload> {
        self.metrics
            .add_count
            .with_label_values(&[&self.peer_label, &self.channel_id])
            .inc();
        self.metrics
            .add_bytes
            .with_label_values(&[&self.peer_label, &self.channel_id])
            .inc_by(message.0.len() as u64);

        match self.send_end.try_send((Instant::now(), message)) {
            Ok(_) => None,
            Err(TrySendError::Full((_, unsent))) => {
                self.metrics
                    .queue_full
                    .with_label_values(&[&self.peer_label, &self.channel_id])
                    .inc();
                Some(unsent)
            }
            Err(TrySendError::Closed((_, unsent))) => {
                self.metrics
                    .no_receiver
                    .with_label_values(&[&self.peer_label, &self.channel_id])
                    .inc();
                Some(unsent)
            }
        }
    }

    fn clear(&mut self) {
        let (send_end, receive_end) = channel(self.queue_size);
        self.send_end = send_end;
        self.receive_end.update(receive_end);
        self.metrics
            .queue_clear
            .with_label_values(&[&self.peer_label, &self.channel_id])
            .inc();
    }
}

/// Send queue implementation
struct SendQueueReaderImpl {
    peer_label: String,
    channel_id: String,
    receive_end_container: Arc<ReceiveEndContainer>,
    cur_receive_end: Option<ReceiveEnd>,
    metrics: SendQueueMetrics,
}

impl SendQueueReaderImpl {
    /// Receives a message with a given timeout. If timeout expires, returns
    /// None.
    async fn receive_with_timeout(
        receive_end: &mut ReceiveEnd,
        timeout: Duration,
    ) -> Option<(Instant, TransportPayload)> {
        let wait_for_entries = async move { receive_end.recv().await };
        match timeout_at(Instant::now() + timeout, wait_for_entries).await {
            // Return None on timeout.
            Err(_) => None,
            // Return None on sender disconnect as well.
            Ok(res) => res,
        }
    }
}

#[async_trait]
impl SendQueueReader for SendQueueReaderImpl {
    async fn dequeue(&mut self, bytes_limit: usize, timeout: Duration) -> Vec<TransportPayload> {
        // The channel end is looked up outside the loop. Any updates
        // to the receive end will be seen only in the next dequeue()
        // call.
        if let Some(receive_end) = self.receive_end_container.take() {
            self.cur_receive_end = Some(receive_end);
            self.metrics
                .receive_end_updates
                .with_label_values(&[&self.peer_label, &self.channel_id])
                .inc();
        }
        let cur_receive_end = self.cur_receive_end.as_mut().unwrap();

        let mut result = Vec::new();
        let mut time_left = timeout; // Initially set to heartbeat timeout.
        let mut removed = 0;
        let mut removed_bytes = 0;
        let mut batch_start_time = Instant::now();
        while let Some((enqueue_time, payload)) =
            Self::receive_with_timeout(cur_receive_end, time_left).await
        {
            self.metrics
                .queue_time_msec
                .with_label_values(&[&self.peer_label, &self.channel_id])
                .observe(enqueue_time.elapsed().as_millis() as f64);
            removed += 1;
            removed_bytes += payload.0.len();

            result.push(payload);

            if removed_bytes >= bytes_limit {
                break;
            }

            // bytes_limit not yet reached
            if removed == 1 {
                // Phase 1 over (heartbeat timeout), start phase 2 with
                // MAX_BATCHING_DURATION_MSEC
                time_left = Duration::from_millis(MAX_BATCHING_DURATION_MSEC);
                batch_start_time = Instant::now();
            } else {
                let batch_duration_msec = batch_start_time.elapsed().as_millis() as u64;
                if batch_duration_msec < MAX_BATCHING_DURATION_MSEC {
                    // Within MAX_BATCHING_DURATION_MSEC, try to batch more
                    time_left =
                        Duration::from_millis(MAX_BATCHING_DURATION_MSEC - batch_duration_msec);
                } else {
                    // Out of time, return what is gathered so far
                    break;
                }
            }
        }

        self.metrics
            .remove_count
            .with_label_values(&[&self.peer_label, &self.channel_id])
            .inc_by(removed as u64);
        self.metrics
            .remove_bytes
            .with_label_values(&[&self.peer_label, &self.channel_id])
            .inc_by(removed_bytes as u64);
        result
    }
}

/// Builds the flow label to use for metrics, from the IP address and the NodeId
pub(crate) fn get_peer_label(node_ip: &str, node_id: &NodeId) -> String {
    // 35: Includes the first 6 groups of 5 chars each + the 5 separators
    let prefix = node_id.to_string().chars().take(35).collect::<String>();
    format!("{}_{}", node_ip, prefix)
}
