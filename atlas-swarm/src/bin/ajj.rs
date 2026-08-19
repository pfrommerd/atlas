use jj_cli::cli_util::CliRunner;

fn main() -> std::process::ExitCode {
    CliRunner::init()
        .name("ajj")
        .about("Jujutsu commands for native Atlas repositories")
        .version("0.44.0-atlas")
        .add_store_factories(atlas_swarm::native_jj::additional_store_factories())
        .run()
        .into()
}
