mod cli;
mod clock;
mod commands;
mod config;
mod devices;
mod exit;
mod heavy;
mod hook;
mod lock;
mod naming;
mod ports;
mod rule;
mod runner;
mod state;
#[cfg(test)]
mod testing;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command, DeviceCommand};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("simporter: {error:#}");
            ExitCode::from(exit::code_for(&error) as u8)
        }
    }
}

fn dispatch(cli: &Cli) -> anyhow::Result<i32> {
    match &cli.command {
        Command::Hook => Ok(hook::run()),
        command => dispatch_with_config(command),
    }
}

fn dispatch_with_config(command: &Command) -> anyhow::Result<i32> {
    let config = config::load()?;
    match command {
        Command::Claim(args) => commands::claim(&config, args),
        Command::Release(args) => commands::release(&config, args),
        Command::Renew(args) => commands::renew(&config, args),
        Command::Status(args) => commands::status(&config, args),
        Command::Device(DeviceCommand::List(args)) => commands::device_list(&config, args),
        Command::Device(DeviceCommand::Add(args)) => commands::device_add(&config, args),
        Command::Device(DeviceCommand::Forget(args)) => commands::device_forget(&config, args),
        Command::Run(args) => runner::run(&config, args),
        Command::Heavy(args) => heavy::run(&config, args),
        Command::Hook => Ok(hook::run()),
        Command::Gc(args) => commands::gc(&config, *args),
        Command::Doctor(args) => Ok(commands::doctor(&config, *args)),
        Command::Rule(args) => rule::run(args),
    }
}
