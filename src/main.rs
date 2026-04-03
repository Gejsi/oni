//! Binary entry point for Oni.
//!
//! The executable keeps only thin orchestration here:
//! - parse CLI arguments
//! - dispatch hidden helper/debug commands
//! - hand real sync work to the library

use std::io;

use clap::Parser;

use oni::cli::{Cli, Command, InternalSubcommand, RunArgs};
use oni::error::OniError;
use oni::manifest::Manifest;
use oni::protocol::{current_implementation, CapabilitySet, Hello, Limits, Message, PeerRole};
use oni::session::{Preview, Request, Summary};
use oni::transport::stdio::Connection;

fn main() -> Result<(), OniError> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Manifest { path }) => {
            let manifest = Manifest::scan(&path)?;

            for entry in manifest.entries {
                println!(
                    "{}\t{}\t{}",
                    entry.kind,
                    entry.metadata.len,
                    entry.path.display()
                );
            }

            Ok(())
        }
        Some(Command::Internal(command)) => match command.command {
            InternalSubcommand::Serve(_) => serve_stdio(),
        },
        None => run(cli.run),
    }
}

fn serve_stdio() -> Result<(), OniError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut connection = Connection::new(
        stdin.lock(),
        stdout.lock(),
        Limits::default().max_frame_bytes() as usize,
    );

    let Some(frame) = connection.receive_frame()? else {
        return Ok(());
    };
    let Message::Hello(remote_hello) = Message::decode(&frame)?;

    let local_hello = Hello::for_current(
        PeerRole::Helper,
        current_implementation(),
        CapabilitySet::default(),
        Limits::default(),
    );

    Hello::negotiate(&local_hello, &remote_hello)?;
    connection.send_frame(&Message::Hello(local_hello).encode()?)?;

    Ok(())
}

fn run(args: RunArgs) -> Result<(), OniError> {
    let options = args.options();
    let source = args
        .source
        .as_deref()
        .ok_or(oni::error::SessionError::MissingOperands)?;
    let destination = args
        .destination
        .as_deref()
        .ok_or(oni::error::SessionError::MissingOperands)?;
    let request = Request::from_args(source, destination, options)?;

    if request.options.dry_run {
        let preview = request.preview()?;
        print_preview(&request, &preview);
    } else {
        let applied = request.apply()?;
        print_apply(&request, &applied);
    }

    Ok(())
}

fn print_preview(request: &Request, preview: &Preview) {
    println!("mode\t{}", preview.mode);
    println!("source\t{}", request.source);
    println!("destination\t{}", request.destination);
    println!("strategy\t{}", request.options.strategy);
    println!("chunker\t{}", request.options.chunker);

    if let Some(tag) = &request.options.benchmark_tag {
        println!("benchmark-tag\t{tag}");
    }

    for operation in &preview.operations {
        println!("{}\t{}", operation.kind, operation.path.display());
    }

    let summary = preview.summary();

    if request.options.stats || request.options.verbose > 0 {
        print_summary(summary);
    }
}

fn print_apply(request: &Request, applied: &Preview) {
    if request.options.verbose > 0 {
        println!("mode\t{}", applied.mode);
        println!("source\t{}", request.source);
        println!("destination\t{}", request.destination);

        for operation in &applied.operations {
            println!("applied\t{}\t{}", operation.kind, operation.path.display());
        }
    }

    if request.options.stats || request.options.verbose > 0 {
        print_summary(applied.summary());
    }
}

fn print_summary(summary: Summary) {
    println!("summary\tcreate={}", summary.create);
    println!("summary\tupdate={}", summary.update_total());
    println!("summary\tupdate-data={}", summary.update_data);
    println!("summary\tupdate-metadata={}", summary.update_metadata);
    println!("summary\tdelete={}", summary.delete);
    println!("summary\tskip={}", summary.skip);
}
