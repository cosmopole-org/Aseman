//! Tests for the per-batch loop-wrapped, single-proof execution path used by
//! the elpify managed VM scheduler.

use elpify_lang::executor::{
    BATCH_MAX_EXPOSED_OUTPUTS, assemble_program, execute_batch_with_proof, stack_outputs_from_ints,
    verify_execution, wrap_masm_in_batch_loop,
};
use elpify_lang::transpile_js_to_masm;

#[test]
fn wrapped_program_assembles_and_has_one_round_per_transaction() {
    let masm = "begin add end";
    let batch = vec![vec![3u64, 4], vec![10, 5], vec![1, 1]];
    let wrapped = wrap_masm_in_batch_loop(masm, &batch).expect("wrap should succeed");
    // One labelled round per transaction.
    assert_eq!(wrapped.matches("round ").count(), 3);
    assemble_program(&wrapped).expect("wrapped MASM must assemble");
}

#[test]
fn raw_masm_batch_resolves_each_transaction_payload() {
    // `add` consumes each transaction's two-element payload and yields its sum.
    let masm = "begin add end";
    let batch = vec![vec![3u64, 4], vec![10, 5], vec![1, 1]];
    let arts = execute_batch_with_proof(masm, &batch).expect("batch should execute");
    assert_eq!(arts.batch_size, 3);
    assert_eq!(arts.per_tx_outputs, vec![7, 15, 2]);
    assert_eq!(&arts.stack_outputs[..3], &[7, 15, 2]);
}

#[test]
fn single_transaction_batch_works() {
    let masm = "begin add end";
    let arts = execute_batch_with_proof(masm, &[vec![20u64, 22]]).expect("execute");
    assert_eq!(arts.batch_size, 1);
    assert_eq!(arts.per_tx_outputs, vec![42]);
}

#[test]
fn batch_proof_verifies() {
    let masm = "begin add end";
    let batch = vec![vec![2u64, 3], vec![4, 5]];
    let arts = execute_batch_with_proof(masm, &batch).expect("execute");
    let outputs = stack_outputs_from_ints(&arts.stack_outputs).expect("outputs convert");
    let security = verify_execution(
        arts.program_info,
        arts.stack_inputs,
        outputs,
        &arts.proof_bytes,
    )
    .expect("batch proof must verify");
    assert!(security >= 96);
}

#[test]
fn transpiled_program_batches_across_rounds() {
    // Transpiled programs ignore operand-stack inputs and compute from literals;
    // every round therefore resolves the same constant — but we still get one
    // round per transaction and a single proof.
    let masm = transpile_js_to_masm("return 6 * 7;").expect("transpile");
    let batch = vec![vec![], vec![], vec![]];
    let arts = execute_batch_with_proof(&masm, &batch).expect("execute");
    assert_eq!(arts.batch_size, 3);
    assert_eq!(arts.per_tx_outputs, vec![42, 42, 42]);
}

#[test]
fn batch_caps_exposed_outputs_but_proves_all_rounds() {
    let masm = "begin add end";
    let n = 20usize;
    let batch: Vec<Vec<u64>> = (0..n as u64).map(|i| vec![i, i]).collect();
    let arts = execute_batch_with_proof(masm, &batch).expect("execute");
    assert_eq!(arts.batch_size, n);
    assert_eq!(arts.per_tx_outputs.len(), BATCH_MAX_EXPOSED_OUTPUTS);
    // Exposed results are transactions 0..15 → sum 2*i.
    for (i, v) in arts.per_tx_outputs.iter().enumerate() {
        assert_eq!(*v, 2 * i as u64);
    }
}

#[test]
fn empty_batch_is_rejected() {
    assert!(wrap_masm_in_batch_loop("begin add end", &[]).is_err());
}
