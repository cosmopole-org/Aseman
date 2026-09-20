fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: run_masm <path>");
        std::process::exit(1);
    }
    let inputs: Vec<u64> = args.iter().skip(2).filter_map(|s| s.parse().ok()).collect();
    match elpify_lang::execute_masm_file_with_proof(&args[1], &inputs) {
        Ok(a) => println!(
            "OK outputs={:?}",
            &a.stack_outputs[..8.min(a.stack_outputs.len())]
        ),
        Err(e) => println!("ERR: {}", e),
    }
}
