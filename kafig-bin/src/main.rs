use kafig::{Result, Sandbox, Process, PreloadSandbox, Command};

fn main() -> Result<()> {
    smol::block_on(async {
        let sandbox = PreloadSandbox::new_auto_lib();
        let mut child = sandbox.spawn(&Command::new("ls")).await?;
        child.wait().await?;
        Ok(())
    })
}