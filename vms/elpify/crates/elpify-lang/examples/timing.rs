fn main() {
    use std::time::Instant;
    let path = "/home/user/masm_samples/hello.masm";

    let t1 = Instant::now();
    let result = elpify_lang::execute_masm_file_with_proof(path, &[]).expect("exec");
    println!("Execute+prove: {:?}", t1.elapsed());

    let t2 = Instant::now();
    let claimed = elpify_lang::stack_outputs_from_ints(&result.stack_outputs).expect("outputs");
    println!("Verify path setup OK in {:?}", t2.elapsed());

    let t3 = Instant::now();
    let security = elpify_lang::verify_execution(
        result.program_info,
        result.stack_inputs,
        claimed,
        &result.proof_bytes,
    )
    .expect("verify");
    println!("Verify proof: {:?}, security={}", t3.elapsed(), security);
}
