//! The command-stream servers: stdio or a local socket → JSON-RPC (`schemify rpc`) or MCP
//! (`schemify mcp`) responses over the headless methods (D27, `docs/spec/rpc.md`).
//!
//! Contents: `fn rpc(&Startup, &[String]) -> Exit`, `fn mcp(&Startup, &[String]) -> Exit`.

use schemify_cli::rpc::{self, Headless};
use schemify_config::Startup;

use crate::{Exit, OK, USAGE_ERROR, usage};

/// `rpc [--listen <socket>]`: the JSON-RPC command stream on stdio until it ends, or on a
/// local socket serving one client after another until the listener fails. Exit 0 at the end
/// of stdin, 2 for usage or I/O errors.
pub fn rpc(start: &Startup, args: &[String]) -> Exit {
    let h = headless(start);
    let served = match args {
        [] => rpc::serve(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            &h,
        ),
        [flag, addr] if flag == "--listen" => rpc::listen(addr).and_then(|listener| {
            // A server: runs until the listener fails; a client's I/O error ends only it.
            loop {
                let mut stream = listener.accept()?;
                let mut input = std::io::BufReader::new(stream.try_clone()?);
                if let Err(e) = rpc::serve(&mut input, &mut stream, &h) {
                    eprintln!("rpc client: {e}");
                }
            }
        }),
        _ => return Err(usage()),
    };
    served.map(|()| OK).map_err(|e| {
        eprintln!("rpc: {e}");
        USAGE_ERROR
    })
}

/// `mcp`: the MCP bridge on stdio until it ends; exit 0 then, 2 for usage or I/O errors.
pub fn mcp(start: &Startup, args: &[String]) -> Exit {
    if !args.is_empty() {
        return Err(usage());
    }
    let h = headless(start);
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    schemify_cli::mcp::serve(&mut stdin.lock(), &mut stdout.lock(), &h)
        .map(|()| OK)
        .map_err(|e| {
            eprintln!("mcp: {e}");
            USAGE_ERROR
        })
}

// ---- private ----

/// The headless methods of this process: this binary runs the CLI ones, with `start`'s flags.
fn headless(start: &Startup) -> Headless {
    Headless {
        exe: std::env::current_exe().ok(),
        flags: start.flags(),
        seed: schemify_cli::platform::seed(),
    }
}
