use std::sync::Arc;
use std::thread;

use elpify_lang::executor::{EngineEvent, EventKind, ExecutionEngine, TaskInput};

#[test]
fn deploy_and_run_single_task_with_events() {
    let engine = ExecutionEngine::new();
    let program_id = engine
        .deploy_program("begin push.0 drop end")
        .expect("deploy should succeed");

    let result = engine
        .submit_task(
            program_id,
            TaskInput {
                // outputs become [1, 1, 7, 99, ...]
                inputs: vec![99, 7, 1, 1],
            },
        )
        .expect("task should succeed");

    assert_eq!(result.runs.len(), 1);
    assert_eq!(result.events, vec![EngineEvent::Set { key: 7, value: 99 }]);
    assert_eq!(result.final_state.get(&7), Some(&99));
}

#[test]
fn batch_tasks_run_sequentially_and_share_state() {
    let engine = ExecutionEngine::new();
    let program_id = engine
        .deploy_program("begin push.0 drop end")
        .expect("deploy should succeed");

    let result = engine
        .submit_batch(
            program_id,
            vec![
                TaskInput {
                    // set key=5 value=10
                    inputs: vec![10, 5, 1, 1],
                },
                TaskInput {
                    // set key=5 value=11
                    inputs: vec![11, 5, 1, 1],
                },
                TaskInput {
                    // delete key=5
                    inputs: vec![0, 5, 2, 1],
                },
            ],
        )
        .expect("batch should succeed");

    assert_eq!(result.runs.len(), 3);
    assert!(!result.final_state.contains_key(&5));
}

#[test]
fn event_handlers_are_invoked() {
    let engine = ExecutionEngine::new();
    engine.register_event_handler(EventKind::Set, |event, state| {
        if let EngineEvent::Set { key, value } = event {
            state.insert(10_000 + *key, value + 1);
        }
    });

    let program_id = engine
        .deploy_program("begin push.0 drop end")
        .expect("deploy should succeed");
    let result = engine
        .submit_task(
            program_id,
            TaskInput {
                inputs: vec![77, 4, 1, 1],
            },
        )
        .expect("task should succeed");

    assert_eq!(result.final_state.get(&4), Some(&77));
    assert_eq!(result.final_state.get(&10004), Some(&78));
}

#[test]
fn different_programs_can_run_in_parallel() {
    let engine = Arc::new(ExecutionEngine::new());
    let p1 = engine
        .deploy_program("begin push.0 drop end")
        .expect("deploy p1");
    let p2 = engine
        .deploy_program("begin push.0 drop end")
        .expect("deploy p2");

    let e1 = Arc::clone(&engine);
    let t1 = thread::spawn(move || {
        e1.submit_batch(
            p1,
            vec![
                TaskInput {
                    inputs: vec![1, 1, 1, 1],
                },
                TaskInput {
                    inputs: vec![2, 2, 1, 1],
                },
            ],
        )
        .expect("p1 batch")
    });

    let e2 = Arc::clone(&engine);
    let t2 = thread::spawn(move || {
        e2.submit_batch(
            p2,
            vec![
                TaskInput {
                    inputs: vec![3, 3, 1, 1],
                },
                TaskInput {
                    inputs: vec![4, 4, 1, 1],
                },
            ],
        )
        .expect("p2 batch")
    });

    let r1 = t1.join().expect("thread1 join");
    let r2 = t2.join().expect("thread2 join");

    assert_eq!(r1.runs.len(), 2);
    assert_eq!(r2.runs.len(), 2);
}
