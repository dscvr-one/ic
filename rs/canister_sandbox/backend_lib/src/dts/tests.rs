use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use ic_interfaces::execution_environment::{
    ExecutionComplexity, HypervisorError, OutOfInstructionsHandler,
};
use ic_types::NumInstructions;

use super::{DeterministicTimeSlicingHandler, PausedExecution};

#[test]
fn dts_state_updates() {
    let mut state = super::State::new(2500, 1000);
    assert_eq!(state.slice_instruction_limit, 1000);
    assert_eq!(state.instructions_executed, 0);
    assert_eq!(state.total_instructions_left(), 2500);
    assert!(!state.is_last_slice());
    state.update(0, ExecutionComplexity::default());
    assert_eq!(state.slice_instruction_limit, 1000);
    assert_eq!(state.instructions_executed, 1000);
    assert_eq!(state.total_instructions_left(), 1500);
    assert!(!state.is_last_slice());
    state.update(0, ExecutionComplexity::default());
    assert_eq!(state.slice_instruction_limit, 500);
    assert_eq!(state.instructions_executed, 2000);
    assert_eq!(state.total_instructions_left(), 500);
    assert!(state.is_last_slice());
    state.update(-500, ExecutionComplexity::default());
    assert_eq!(state.slice_instruction_limit, 0);
    assert_eq!(state.instructions_executed, 3000);
    assert_eq!(state.total_instructions_left(), -500);
    assert!(state.is_last_slice());
}

#[test]
fn dts_state_updates_invalid_instructions() {
    let mut state = super::State::new(2500, 2000);
    assert_eq!(state.slice_instruction_limit, 2000);
    assert_eq!(state.instructions_executed, 0);
    assert_eq!(state.total_instructions_left(), 2500);
    assert!(!state.is_last_slice());
    state.update(4000, ExecutionComplexity::default());
    assert_eq!(state.slice_instruction_limit, 2000);
    assert_eq!(state.instructions_executed, 0);
    assert_eq!(state.total_instructions_left(), 2500);
    assert!(!state.is_last_slice());
}

#[test]
fn dts_state_updates_saturating() {
    let mut state = super::State::new(2500, 2000);
    assert_eq!(state.slice_instruction_limit, 2000);
    assert_eq!(state.instructions_executed, 0);
    assert_eq!(state.total_instructions_left(), 2500);
    assert!(!state.is_last_slice());
    state.update(i64::MIN, ExecutionComplexity::default());
    assert_eq!(state.slice_instruction_limit, 0);
    assert_eq!(state.instructions_executed, i64::MAX);
    assert_eq!(state.total_instructions_left(), 2500 - i64::MAX);
    assert!(state.is_last_slice());
}

#[test]
fn pause_and_resume_works() {
    let (tx, rx): (Sender<PausedExecution>, Receiver<PausedExecution>) = mpsc::channel();
    let dts = DeterministicTimeSlicingHandler::new(2500, 1000, move |_slice, paused| {
        tx.send(paused).unwrap();
    });
    let control_thread = thread::spawn(move || {
        for _ in 0..2 {
            let paused_execution = rx.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
            paused_execution.resume();
        }
    });
    // Slice 1: executes 1000 instructions before calling `out_of_instructions()`.
    let next_slice_limit = dts.out_of_instructions(0, Default::default()).unwrap();
    assert_eq!(1000, next_slice_limit);
    // Slice 2: executes 1000 instructions before calling `out_of_instructions()`.
    let next_slice_limit = dts.out_of_instructions(0, Default::default()).unwrap();
    assert_eq!(500, next_slice_limit);
    // Slice 3: executes 500 instructions before calling `out_of_instructions()`.
    let error = dts.out_of_instructions(0, Default::default());
    assert_eq!(error, Err(HypervisorError::InstructionLimitExceeded));
    drop(dts);
    control_thread.join().unwrap();
}

#[test]
fn early_exit_if_slice_does_not_any_instructions_left() {
    let (tx, rx): (Sender<PausedExecution>, Receiver<PausedExecution>) = mpsc::channel();
    let dts = DeterministicTimeSlicingHandler::new(10000, 1000, move |_slice, paused| {
        tx.send(paused).unwrap();
    });
    let control_thread = thread::spawn(move || {
        let paused_execution = rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        paused_execution.resume();
    });
    // Slice 1: executes 1500 instructions before calling `out_of_instructions()`.
    let new_slice_limit = dts.out_of_instructions(-500, Default::default()).unwrap();
    assert_eq!(500, new_slice_limit);
    // Slice 2: executes 1500 instructions before calling `out_of_instructions()`
    // and fails because the next slice wouldn't have any slice instructions left.
    let error = dts.out_of_instructions(-1000, Default::default());
    assert_eq!(
        error,
        Err(HypervisorError::SliceOverrun {
            instructions: NumInstructions::from(1500),
            limit: NumInstructions::from(1000)
        })
    );
    drop(dts);
    control_thread.join().unwrap();
}

#[test]
fn invalid_instructions() {
    let (tx, rx): (Sender<PausedExecution>, Receiver<PausedExecution>) = mpsc::channel();
    let dts = DeterministicTimeSlicingHandler::new(2500, 1000, move |_slice, paused| {
        tx.send(paused).unwrap();
    });
    let control_thread = thread::spawn(move || {
        for _ in 0..2 {
            let paused_execution = rx.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
            paused_execution.resume();
        }
    });
    // Slice 1: executes 1000 instructions before calling `out_of_instructions()`.
    let new_instructions = dts.out_of_instructions(0, Default::default()).unwrap();
    assert_eq!(1000, new_instructions);
    // Slice 2: executes 0 instructions before calling `out_of_instructions()`.
    let new_instructions = dts
        .out_of_instructions(i64::MAX, Default::default())
        .unwrap();
    assert_eq!(1000, new_instructions);
    // Slice 3: executes more than i64::MAX instructions before calling `out_of_instructions()`.
    let error = dts.out_of_instructions(i64::MIN, Default::default());
    assert_eq!(
        error,
        Err(HypervisorError::SliceOverrun {
            instructions: NumInstructions::from(9223372036854775807),
            limit: NumInstructions::from(1000)
        })
    );
    drop(dts);
    control_thread.join().unwrap();
}
