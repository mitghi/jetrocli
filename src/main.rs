use clap::Parser;
use jetro::context::{Path, PathResult};
use serde_json;
use serde_json::Value;
use std::fs;
use std::io::{self, Read};

#[derive(Parser, Debug)]
#[command(name = "Jetro CLI")]
#[command(version = "v0.1.0")]
#[command(author = "Mike Taghavi <mitghi[at]me.com>")]
#[command(about = "Transform, compare and query JSON format")]
struct Config {
    #[arg(short, long)]
    #[arg(help = "Jetro query")]
    query: String,

    #[arg(short, long)]
    #[arg(help = "JSON filepath ( or pipe to stdin instead )")]
    file: Option<String>,
}

struct Cli {
    config: Config,
}

impl Cli {
    fn new() -> Self {
        Self {
            config: Config::parse(),
        }
    }

    fn eval(&mut self) -> Result<PathResult, Box<dyn std::error::Error>> {
        let input: String = if self.config.file.is_none() {
            self.read_input()?
        } else {
            self.read_file()?
        };

        let json: Value = serde_json::from_str(&input)?;
        match Path::collect(json, self.config.query.clone()) {
            Ok(output) => Ok(output),
            Err(err) => Err(err.into()),
        }
    }

    fn read_file(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        match fs::read_to_string(self.config.file.clone().unwrap()) {
            Ok(output) => Ok(output),
            Err(err) => Err(err.into()),
        }
    }

    fn read_input(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let mut input = String::new();

        match stdin.read_to_string(&mut input) {
            Ok(_num_bytes) => {
                debug_assert!(!input.is_empty());
                let input = &input;
                Ok(input
                    .strip_prefix("\n")
                    .or(input.strip_suffix("\n"))
                    .unwrap_or(input)
                    .to_string())
            }
            Err(err) => Err(err.into()),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::new().eval() {
        Ok(output) => {
            if output.0.borrow().len() == 0 {
                println!("Nothing!");
                return Ok(());
            }
            for value in output.0.borrow().iter() {
                println!("{}", serde_json::to_string_pretty(&*value.clone()).unwrap());
            }
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}
