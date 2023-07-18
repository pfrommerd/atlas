use atlas_sandbox::fuse::FuseServer;
use atlas_sandbox::fs::local::LocalFS;
use fuser::MountOption;
use futures_lite::future;

use clap::{Arg, Command, ArgMatches};
use smol::stream::StreamExt;
use smol::LocalExecutor;

use log::info;

async fn run(args: ArgMatches) {
    let fs_dir : &String = args.get_one::<String>("src-dir").unwrap();
    let mount_point : &String = args.get_one::<String>("mount-point").unwrap();
    info!("Creating LocalFS");
    let fs = LocalFS::new(fs_dir.into());
    info!("Starting fuse server");
    let server = FuseServer::new(
        &mount_point, &fs,
        &[MountOption::FSName("atlas".to_string())]
    ).unwrap();
    info!("Handling fuse events");
    let executor = LocalExecutor::new();
    let mut tasks = Vec::new();

    // spawn 10 tasks
    // this allows for parallel handling of fuse events
    for _ in 0..10 {
        tasks.push(executor.spawn(server.run()));
    }
    // run the handling tasks
    executor.run(async move {
        for task in tasks {
            task.await.unwrap();
        }
    }).await;
}

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Trace)
        .init();

    let cmd = Command::new("atlas-sandbox")
        .version("0.0.1")
        .author("Daniel Pfrommer")
        .arg(
            Arg::new("src-dir")
                .help("The source point for the file system")
                .required(true)
        )
        .arg(
            Arg::new("mount-point")
                .help("The mount point for the file system")
                .required(true)
        );
    let args = cmd.try_get_matches().unwrap_or_else(|e| e.exit());
    let executor = LocalExecutor::new();
    future::block_on(executor.run(async {
        let mut signals = async_signals::Signals::new(vec![libc::SIGINT]).unwrap();
        let task = executor.spawn(run(args));
        signals.next().await;
        info!("Shutting down fuse server");
        task.cancel().await;
    }));
}