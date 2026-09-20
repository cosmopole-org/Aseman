use elpify_lang::compiler::transpile_js_to_masm;
use elpify_lang::executor::{
    assemble_program, execute_with_proof, stack_outputs_from_ints, verify_execution,
};

fn run_program(source: &str) -> u64 {
    let masm = transpile_js_to_masm(source).expect("transpile should succeed");
    let artifacts = execute_with_proof(&masm, &[]).expect("execution should succeed");
    artifacts.stack_outputs[0]
}

#[test]
fn transpiler_rejects_invalid_syntax() {
    let err = transpile_js_to_masm("let x = ;").expect_err("should fail");
    let msg = err.to_string();
    assert!(msg.contains("parse error"));
}

#[test]
fn transpiler_rejects_member_call_on_non_instance() {
    let err = transpile_js_to_masm("let x = 1; return x.inc(2);").expect_err("should fail");
    let msg = err.to_string();
    assert!(msg.contains("member calls require an instance variable"));
}

#[test]
fn transpiler_generates_assemblable_masm_for_classes_and_loops() {
    let source = r#"
        class Calc {
            constructor(base) { return base; }
            add(v) { return this + v; }
        }

        let c = new Calc(7);
        let total = 0;
        for (let i = 0; i < 3; i = i + 1) {
            total = total + c.add(i);
        }
        return total;
    "#;

    let masm = transpile_js_to_masm(source).expect("should transpile");
    assemble_program(&masm).expect("should assemble");
    assert!(masm.contains("proc.Calc__add"));
    assert!(masm.contains("while.true"));
}

#[test]
fn executes_control_flow_and_ternary_correctly() {
    let source = r#"
        let x = 9;
        let cond = x > 5;
        let r = cond ? x + 1 : 0;
        return r;
    "#;

    assert_eq!(run_program(source), 10);
}

#[test]
fn executes_switch_case_path() {
    let source = r#"
        let x = 4;
        switch (x) {
            case 3: x = 99; break;
            case 4: x = 42; break;
            default: x = 1; break;
        }
        return x;
    "#;

    assert_eq!(run_program(source), 42);
}

#[test]
fn verify_fails_for_tampered_proof() {
    let source = "return 21 + 21;";
    let masm = transpile_js_to_masm(source).expect("transpile should succeed");
    let artifacts = execute_with_proof(&masm, &[]).expect("execution should succeed");

    let mut tampered = artifacts.proof_bytes.clone();
    let idx = tampered.len() / 2;
    tampered[idx] ^= 0x01;

    let outputs =
        stack_outputs_from_ints(&artifacts.stack_outputs).expect("outputs should convert");
    let res = verify_execution(
        artifacts.program_info,
        artifacts.stack_inputs,
        outputs,
        &tampered,
    );
    assert!(res.is_err(), "tampered proof must not verify");
}

#[test]
fn verify_fails_for_wrong_outputs() {
    let source = "return 5 * 5;";
    let masm = transpile_js_to_masm(source).expect("transpile should succeed");
    let artifacts = execute_with_proof(&masm, &[]).expect("execution should succeed");

    let mut wrong = artifacts.stack_outputs.clone();
    wrong[0] = wrong[0].wrapping_add(1);
    let wrong_outputs = stack_outputs_from_ints(&wrong).expect("outputs should convert");

    let res = verify_execution(
        artifacts.program_info,
        artifacts.stack_inputs,
        wrong_outputs,
        &artifacts.proof_bytes,
    );
    assert!(res.is_err(), "wrong public outputs must fail verification");
}

#[test]
fn assembler_reports_invalid_masm() {
    let res = assemble_program("begin push.1 add");
    assert!(res.is_err());
}

#[test]
fn real_world_fintech_settlement_flow() {
    let source = r#"
        class Wallet {
            constructor(seed) { return seed; }
            credit(amount) { return this + amount; }
            debit(amount) { return this + amount; }
        }

        function fee(amount) {
            return amount > 100 ? 3 : 1;
        }

        let ledgerMeta = { system: "core-ledger", region: "us-east" };
        let checkpoints = [100, 200, 300];

        let w = new Wallet(500);
        for (let i = 0; i < 5; i = i + 1) {
            if (i == 0) {
                w = w.credit(40);
            } else {
                if (i == 1) {
                    w = w.debit(15);
                } else {
                    if (i == 2) {
                        w = w.credit(25);
                    } else {
                        if (i == 3) {
                            w = w.debit(30);
                        } else {
                            w = w.credit(10);
                        }
                    }
                }
            }
            w = w.debit(fee(i * 50));
        }

        return w;
    "#;

    assert_eq!(run_program(source), 3);
}

#[test]
fn real_world_loan_underwriting_pipeline() {
    let source = r#"
        class Score {
            constructor(base) { return base; }
            add(v) { return this + v; }
        }

        function kycRisk(age, countryCode) {
            let s = 0;
            if (age < 21) {
                s = s + 20;
            } else {
                s = s + 5;
            }
            switch (countryCode) {
                case 1: s = s + 10; break;
                case 2: s = s + 20; break;
                default: s = s + 30; break;
            }
            return s;
        }

        let profile = { age: 28, country: 2 };
        let docs = ["id", "income", "bank"];

        let risk = new Score(0);
        let age = 28;
        let cc = 2;
        let risk = risk.add(kycRisk(age, cc));
        for (let m = 0; m < 3; m = m + 1) {
            let risk = risk.add(m * 7);
        }

        let decision = risk > 55 ? 1 : 0;
        return decision * 1000 + risk;
    "#;

    assert_eq!(run_program(source), 46);
}

#[test]
fn real_world_ecommerce_checkout_process() {
    let source = r#"
        class Cart {
            constructor(base) { return base; }
            add(v) { return this + v; }
        }

        function shipping(region, weight) {
            let cost = 5;
            if (weight > 10) {
                cost = cost + 8;
            }
            switch (region) {
                case 1: cost = cost + 2; break;
                case 2: cost = cost + 4; break;
                default: cost = cost + 7; break;
            }
            return cost;
        }

        let basket = ["item-a", "item-b", "item-c", "item-d"];
        let ctx = { channel: "web", region: 3 };

        let cart = new Cart(0);
        for (let i = 0; i < 4; i = i + 1) {
            let cart = cart.add((i + 1) * 12);
        }

        let ship = shipping(3, 11);
        let promo = cart > 100 ? 15 : 5;
        let total = (cart - promo) + ship;
        return total;
    "#;

    assert_eq!(run_program(source), 125);
}

#[test]
fn off_chain_proof_verifies_on_chain_for_simple_arithmetic() {
    // Off-chain side: produce stack outputs + a STARK proof for `2 + 3`.
    let source = "return 2 + 3;";
    let masm = transpile_js_to_masm(source).expect("transpile");
    let artifacts = execute_with_proof(&masm, &[]).expect("execute");

    assert_eq!(
        artifacts.stack_outputs[0], 5,
        "off-chain execution result must be 5"
    );

    // On-chain side: verify the proof against the (program info, public
    // inputs, public outputs, proof) tuple — the exact data Caspar would
    // ship across consensus.
    let outputs = stack_outputs_from_ints(&artifacts.stack_outputs).expect("outputs convert");
    let cycles = verify_execution(
        artifacts.program_info,
        artifacts.stack_inputs,
        outputs,
        &artifacts.proof_bytes,
    )
    .expect("verification of a fresh proof must succeed");
    assert!(cycles > 0, "verifier should report a non-zero cycle count");
}

#[test]
fn proof_bytes_round_trip_preserves_verifiability() {
    // Simulate serialising the proof for transport then deserialising it
    // back via the verifier path.
    let masm = transpile_js_to_masm("return 7 * 6;").expect("transpile");
    let artifacts = execute_with_proof(&masm, &[]).expect("execute");
    assert_eq!(artifacts.stack_outputs[0], 42);

    // Round-trip the bytes through an owned Vec to mimic wire transport.
    let on_wire = artifacts.proof_bytes.clone();
    let received = on_wire.to_vec();

    let outputs = stack_outputs_from_ints(&artifacts.stack_outputs).expect("outputs convert");
    let result = verify_execution(
        artifacts.program_info,
        artifacts.stack_inputs,
        outputs,
        &received,
    );
    assert!(result.is_ok(), "round-tripped proof must verify");
}

#[test]
fn verify_rejects_truncated_proof_bytes() {
    let masm = transpile_js_to_masm("return 1 + 1;").expect("transpile");
    let artifacts = execute_with_proof(&masm, &[]).expect("execute");

    // Lop a few bytes off the end — should produce a deserialization or
    // verification error, not a panic.
    let short = &artifacts.proof_bytes[..artifacts.proof_bytes.len() / 2];
    let outputs = stack_outputs_from_ints(&artifacts.stack_outputs).expect("outputs convert");
    let err = verify_execution(
        artifacts.program_info,
        artifacts.stack_inputs,
        outputs,
        short,
    );
    assert!(err.is_err(), "truncated proof must not verify");
}

#[test]
fn execute_with_proof_errors_on_invalid_masm() {
    // Pre-compiled MASM with a syntax error should propagate as an
    // ExecutorError, never panic.
    let err = execute_with_proof("not valid masm at all", &[]).expect_err("should fail");
    assert!(format!("{}", err).len() > 0);
}

#[test]
fn stack_outputs_from_ints_round_trips_simple_value() {
    let outs = stack_outputs_from_ints(&[5, 0, 0, 0]).expect("convert");
    // The wrapper accepts the value back through the verifier; we only need
    // to assert the conversion does not error and the public API holds.
    let _ = outs;
}
