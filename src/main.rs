use clap::Parser;

use oni::cli::{Cli, Command, InternalSubcommand, RunArgs};
use oni::error::OniError;
use oni::manifest::Manifest;
use oni::session::{Preview, Request};

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
            InternalSubcommand::Serve(_) => {
                Err(oni::error::SessionError::ServeNotImplemented.into())
            }
        },
        None => run(cli.run),
    }
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
        println!("summary\tcreate={}", summary.create);
        println!("summary\tupdate={}", summary.update_total());
        println!("summary\tupdate-data={}", summary.update_data);
        println!("summary\tupdate-metadata={}", summary.update_metadata);
        println!("summary\tdelete={}", summary.delete);
        println!("summary\tskip={}", summary.skip);
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
        let summary = applied.summary();

        println!("summary\tcreate={}", summary.create);
        println!("summary\tupdate={}", summary.update_total());
        println!("summary\tupdate-data={}", summary.update_data);
        println!("summary\tupdate-metadata={}", summary.update_metadata);
        println!("summary\tdelete={}", summary.delete);
        println!("summary\tskip={}", summary.skip);
    }
}
