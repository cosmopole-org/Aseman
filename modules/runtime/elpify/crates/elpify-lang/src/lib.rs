//! `elify-lang` library entrypoint.
//!
//! This crate exposes:
//! - A JS-like to MASM transpiler.
//! - MASM execution/proving/verification helpers.
//! - A deployable execution engine with task queues and event handlers.

pub mod compiler;
pub mod executor;

pub use compiler::{CompilerError, transpile_js_to_masm};
pub use executor::{
    BATCH_MAX_EXPOSED_OUTPUTS, BatchArtifacts, EngineError, EngineEvent, EventKind,
    ExecutionArtifacts, ExecutionEngine, ExecutorError, ProgramId, TaskInput, TaskResult,
    assemble_program, execute_batch_with_proof, execute_masm_file_batch_with_proof,
    execute_masm_file_with_proof, execute_with_proof, stack_outputs_from_ints, verify_execution,
    wrap_masm_in_batch_loop,
};

#[cfg(test)]
mod tests {
    use crate::compiler::transpile_js_to_masm;
    use crate::executor::{
        assemble_program, execute_with_proof, stack_outputs_from_ints, verify_execution,
    };

    #[test]
    fn transpiles_feature_rich_javascript_syntax_to_masm() {
        let source = r#"
            function add(a, b) {
                let sum = a + b;
                return sum;
            }

            let x = 2;
            let y = 3;

            if (x < y) {
                x = add(x, y);
            } else {
                x = 0;
            }

            let i = 0;
            while (i < 3) {
                x = x + 2;
                i = i + 1;
            }

            for (let j = 0; j < 2; j = j + 1) {
                x = x + 1;
                continue;
            }

            switch (x) {
                case 11:
                    x = x + 5;
                    break;
                default:
                    x = x + 1;
                    break;
            }

            let label = "miden";
            let arr = [1, 2, 3, true, "x"];
            let obj = { name: "elify", ok: true };

            let signed = -x;
            return signed < 0 ? x : 0;
        "#;

        let masm = transpile_js_to_masm(source).expect("should transpile rich syntax");
        assert!(masm.contains("proc.add"));
        assert!(masm.contains("if.true"));
        assert!(masm.contains("while.true"));
        assert!(masm.contains("neg"));
        assert!(masm.contains("exec.add"));
        assert!(masm.contains("loc_store"));

        assemble_program(&masm).expect("MASM should assemble");
    }

    #[test]
    fn supports_js_data_type_literals() {
        let source = r#"
            let s = "hello";
            let b = true;
            let arr = [1, 2, 3];
            let obj = { name: "miden", ok: true };

            let r = 0;
            if (s != null && b) {
                r = arr == arr;
            }
            return r;
        "#;

        let masm = transpile_js_to_masm(source).expect("should transpile data type literals");
        let artifacts = execute_with_proof(&masm, &[]).expect("should execute");
        assert_eq!(artifacts.stack_outputs[0], 1);
    }

    #[test]
    fn supports_class_constructor_and_method_calls() {
        let source = r#"
            class Counter {
                constructor(base) { return base; }
                inc(step) { return this + step; }
            }

            let c = new Counter(10);
            return c.inc(5);
        "#;

        let masm = transpile_js_to_masm(source).expect("should transpile class syntax");
        assert!(masm.contains("proc.Counter__constructor"));
        assert!(masm.contains("proc.Counter__inc"));
        assert!(masm.contains("exec.Counter__inc"));

        let artifacts = execute_with_proof(&masm, &[]).expect("should execute class-based program");
        assert_eq!(artifacts.stack_outputs[0], 15);
    }

    #[test]
    fn executes_and_verifies_proof() {
        let source = r#"
            function mul_add(a, b, c) {
                let t = a * b;
                return t + c;
            }

            let x = 6;
            let y = 7;
            let z = 1;
            return mul_add(x, y, z);
        "#;

        let masm = transpile_js_to_masm(source).expect("should transpile");
        let artifacts = execute_with_proof(&masm, &[]).expect("should execute and prove");

        assert_eq!(artifacts.stack_outputs[0], 43);

        let claimed_outputs = stack_outputs_from_ints(&artifacts.stack_outputs)
            .expect("should convert outputs into StackOutputs");

        let security = verify_execution(
            artifacts.program_info,
            artifacts.stack_inputs,
            claimed_outputs,
            &artifacts.proof_bytes,
        )
        .expect("proof should verify");

        assert!(security >= 96);
    }

    #[test]
    fn executes_raw_masm_with_stack_inputs() {
        let masm = "begin add end";
        let artifacts =
            execute_with_proof(masm, &[3, 4]).expect("should execute with stack inputs");
        assert_eq!(artifacts.stack_outputs[0], 7);
    }
}
