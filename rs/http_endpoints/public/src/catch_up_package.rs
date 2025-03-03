//! Module that deals with requests to /_/catch_up_package

use crate::{
    body::BodyReceiverLayer, common, types::ApiReqType, EndpointService, HttpHandlerMetrics,
    UNKNOWN_LABEL,
};
use http::Request;
use hyper::{Body, Response, StatusCode};
use ic_interfaces::consensus_pool::ConsensusPoolCache;
use ic_types::consensus::catchup::CatchUpPackageParam;
use prost::Message;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{
    limit::concurrency::GlobalConcurrencyLimitLayer, util::BoxCloneService, Service, ServiceBuilder,
};

const MAX_CATCH_UP_PACKAGE_CONCURRENT_REQUESTS: usize = 100;

#[derive(Clone)]
pub(crate) struct CatchUpPackageService {
    metrics: HttpHandlerMetrics,
    consensus_pool_cache: Arc<dyn ConsensusPoolCache>,
}

impl CatchUpPackageService {
    pub(crate) fn new_service(
        metrics: HttpHandlerMetrics,
        consensus_pool_cache: Arc<dyn ConsensusPoolCache>,
    ) -> EndpointService {
        let base_service = BoxCloneService::new(
            ServiceBuilder::new()
                .layer(GlobalConcurrencyLimitLayer::new(
                    MAX_CATCH_UP_PACKAGE_CONCURRENT_REQUESTS,
                ))
                .service(Self {
                    metrics,
                    consensus_pool_cache,
                }),
        );

        BoxCloneService::new(
            ServiceBuilder::new()
                .layer(BodyReceiverLayer::default())
                .service(base_service),
        )
    }
}

/// Write the provided prost::Message as a serialized protobuf into a Response
/// object.
fn protobuf_response<R: Message>(r: &R) -> Response<Body> {
    use hyper::header;
    let mut buf = Vec::<u8>::new();
    r.encode(&mut buf)
        .expect("impossible: Serialization failed");
    let mut response = Response::new(Body::from(buf));
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = common::get_cors_headers();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(common::CONTENT_TYPE_PROTOBUF),
    );
    response
}

impl Service<Request<Vec<u8>>> for CatchUpPackageService {
    type Response = Response<Body>;
    type Error = Infallible;
    #[allow(clippy::type_complexity)]
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Vec<u8>>) -> Self::Future {
        self.metrics
            .request_body_size_bytes
            .with_label_values(&[ApiReqType::CatchUpPackage.into(), UNKNOWN_LABEL])
            .observe(request.body().len() as f64);

        let body = request.into_body();
        let cup = self.consensus_pool_cache.cup_with_protobuf();
        let res = if body.is_empty() {
            Ok(protobuf_response(&cup.protobuf))
        } else {
            match serde_cbor::from_slice::<CatchUpPackageParam>(&body) {
                Ok(param) => {
                    if CatchUpPackageParam::from(&cup.cup) > param {
                        Ok(protobuf_response(&cup.protobuf))
                    } else {
                        Ok(common::empty_response())
                    }
                }
                Err(e) => Ok(common::make_plaintext_response(
                    StatusCode::BAD_REQUEST,
                    format!("Could not parse body as CatchUpPackage param: {}", e),
                )),
            }
        };
        Box::pin(async move { res })
    }
}
