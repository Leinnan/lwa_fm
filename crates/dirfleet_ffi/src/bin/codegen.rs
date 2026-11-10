//! Writes the Swift types of the core's events and view model, and of the
//! effects shells handle (`dirfleet_ffi::Effect`, not the app's full list).
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crux_core::type_generation::facet::{Config, Export as _, TypeRegistry};
use dirfleet_core::app::{Event, ViewModel};

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    pretty_env_logger::init();
    let args = Args::parse();
    let mut registry = TypeRegistry::new();
    dirfleet_ffi::Effect::register_types(&mut registry)?;
    let registry = registry
        .register_type::<Event>()?
        .register_type::<ViewModel>()?
        .build()?;
    registry.swift(&Config::builder("App", &args.output_dir).build())?;
    Ok(())
}
