use std::env;

pub struct AppArgs {
    pub target_pid: Option<i32>,
    pub cmd_name: Option<String>,
    pub cmd_args: Vec<String>,
    pub output_file: Option<String>,
    pub trace_filter: Option<String>,
    pub script_file: Option<String>,
    pub custom_dylib: Option<String>,
    pub json_output: bool,
    pub ecs_output: bool,
    pub ndump: bool,
}

pub fn parse_args() -> AppArgs {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut parsed = AppArgs {
        target_pid: None,
        cmd_name: None,
        cmd_args: Vec::new(),
        output_file: None,
        trace_filter: None,
        script_file: None,
        custom_dylib: None,
        json_output: false,
        ecs_output: false,
        ndump: false,
    };

    if args.is_empty() {
        print_help();
        std::process::exit(1);
    }

    while !args.is_empty() {
        if args[0] == "-o" || args[0] == "--output" {
            args.remove(0);
            if args.is_empty() {
                eprintln!("Error: -o requires a file path");
                std::process::exit(1);
            }
            parsed.output_file = Some(args.remove(0));
        } else if args[0] == "-t" || args[0] == "--trace" {
            args.remove(0);
            if args.is_empty() {
                eprintln!("Error: -t requires a comma-separated list of syscalls");
                std::process::exit(1);
            }
            parsed.trace_filter = Some(args.remove(0));
        } else if args[0] == "-p" || args[0] == "--pid" {
            args.remove(0);
            if args.is_empty() {
                eprintln!("Error: -p requires a PID");
                std::process::exit(1);
            }
            parsed.target_pid = Some(args.remove(0).parse().expect("Invalid PID."));
        } else if args[0] == "-j" || args[0] == "--json" {
            args.remove(0);
            parsed.json_output = true;
        } else if args[0] == "-e" || args[0] == "--ecs" {
            args.remove(0);
            parsed.ecs_output = true;
        } else if args[0] == "-l" || args[0] == "--load" {
            args.remove(0);
            if args.is_empty() {
                eprintln!("Error: -l requires a path to a dylib");
                std::process::exit(1);
            }
            parsed.custom_dylib = Some(args.remove(0));
        } else if args[0] == "-s" || args[0] == "--script" || args[0] == "--sandboxed" || args[0] == "--swap" {
            args.remove(0);
            if args.is_empty() {
                eprintln!("Error: -s requires a safe Rust probe script (.rs)");
                std::process::exit(1);
            }
            parsed.script_file = Some(args.remove(0));
        } else if args[0] == "--ndump" || args[0] == "-ndump" {
            args.remove(0);
            parsed.ndump = true;
        } else if args[0] == "-h" || args[0] == "-help" || args[0] == "--help" {
            print_help();
            std::process::exit(0);
        } else if args[0].starts_with("-") {
            eprintln!("Unknown argument: {}", args[0]);
            eprintln!("Use -h or --help for usage information.");
            std::process::exit(1);
        } else {
            break;
        }
    }

    if parsed.target_pid.is_none() {
        if args.is_empty() {
            eprintln!("Error: No command specified to trace.");
            std::process::exit(1);
        }
        parsed.cmd_name = Some(args.remove(0));
        parsed.cmd_args = args;
    }

    parsed
}

pub fn print_help() {
    println!("mtdi - High-speed macOS user-space dynamic instrumentation");
    println!("");
    println!("Usage: mtdi [OPTIONS] <command> [args...]");
    println!("       mtdi -p <PID>");
    println!("");
    println!("Options:");
    println!("  -s, --script <file.rs> Compile, sandbox-verify, and inject a safe Rust probe");
    println!("  -l, --load <dylib>     Load a custom pre-compiled dylib directly (BYOB)");
    println!("  -p, --pid <PID>        Attach to an already running process");
    println!("  -t, --trace <calls>    Comma-separated list of syscalls to intercept (e.g. open,read)");
    println!("  -o, --output <file>    Write output to a specific file instead of stderr");
    println!("  -j, --json             Export logs in NDJSON format");
    println!("  -e, --ecs              Export logs in Elastic Common Schema (ECS) JSON format");
    println!("  -h, --help             Print this help message and exit");
    println!("");
    println!("Examples:");
    println!("  mtdi -s my_probe.rs curl http://example.com");
    println!("  mtdi -s my_probe.rs -p 1234");
    println!("  mtdi -l custom_hook.dylib -p 1234");
}
