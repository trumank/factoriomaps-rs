use std::fs::{self, File};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use fs2::FileExt;
use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};

static MOD: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/mod");

#[derive(Debug, Serialize, Deserialize)]
struct FactorioMods {
    mods: Vec<FactorioModEntry>,
}
#[derive(Debug, Serialize, Deserialize)]
struct FactorioModEntry {
    name: String,
    enabled: bool,
}

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    Render(ActionRender),
}

#[derive(Parser)]
struct ActionRender {
    /// Factorio directory root
    factorio: PathBuf,
    /// Render output path
    output: PathBuf,
    /// Path to map to render
    map: String,
    /// By default Xvfb will be used to run factorio in the background. Set this flag to make
    /// the window visible
    #[clap(long, short)]
    debug: bool,
}

fn main() {
    let args = Args::parse().action;
    match args {
        Action::Render(action) => {
            render(action);
        }
    }
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        match self.kill() {
            Err(ref e) if e.kind() == std::io::ErrorKind::InvalidInput => {} // already exited, do nothing
            Err(e) => println!("Could not kill child process: {e}"),
            Ok(_) => println!("Successfully killed child process"),
        }
    }
}
impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

struct SetupGuard {
    mod_path: PathBuf,
    lib_path: PathBuf,
    modlist_path: PathBuf,
    modlist_str: String,
}
impl SetupGuard {
    fn new<P: AsRef<Path>>(factorio: P, output: P, map: &str) -> Self {
        // check factorio lockfile
        if let Ok(lockfile) = File::open(factorio.as_ref().join(".lock")) {
            lockfile
                .try_lock_exclusive()
                .expect("Could not open lockfile, is factorio already running?");
            lockfile.unlock().unwrap();
        }

        let mut sync_mods = ChildGuard(
            std::process::Command::new(factorio.as_ref().join("bin/x64/factorio"))
                .arg("--sync-mods")
                .arg(map)
                .spawn()
                .unwrap(),
        );
        sync_mods.wait().unwrap();

        // insert self into factorio mod list and save original to restore later
        let modname = "factoriomaps-rs";
        let modlist_path = factorio.as_ref().join("mods/mod-list.json");
        let modlist_str = fs::read_to_string(&modlist_path).unwrap();
        let mut modlist: FactorioMods = serde_json::from_str(&modlist_str).unwrap();
        let mut found = false;
        for entry in &mut modlist.mods {
            if entry.name == modname {
                entry.enabled = true;
                found = true;
                break;
            }
        }
        if !found {
            modlist.mods.push(FactorioModEntry {
                name: modname.to_owned(),
                enabled: true,
            });
        }
        fs::write(&modlist_path, serde_json::to_vec_pretty(&modlist).unwrap()).unwrap();
        let mod_path = factorio.as_ref().join("mods").join(modname);
        fs::remove_dir_all(&mod_path).ok();
        fs::create_dir(&mod_path).unwrap();
        MOD.extract(&mod_path).unwrap();
        let lib_path = mod_path.join("libfactoriomaps_lib.so");
        fs::write(
            &lib_path,
            include_bytes!(env!("CARGO_CDYLIB_FILE_FACTORIOMAPS_LIB")),
        )
        .unwrap();

        std::fs::create_dir_all(output).unwrap();

        Self {
            modlist_path,
            modlist_str,
            mod_path,
            lib_path,
        }
    }
}
impl Drop for SetupGuard {
    fn drop(&mut self) {
        fs::write(&self.modlist_path, self.modlist_str.as_bytes()).unwrap();
        fs::remove_dir_all(&self.mod_path).unwrap();
    }
}

fn render(action: ActionRender) {
    crossbeam::scope(|_| {
        let ActionRender {
            factorio,
            output,
            map,
            debug,
        } = action;
        let setup_guard = SetupGuard::new(&factorio, &output, &map);

        let mut factorio_cmd = std::process::Command::new(factorio.join("bin/x64/factorio"));

        let _xvfb = if !debug {
            factorio_cmd.env("DISPLAY", ":8");

            Some(ChildGuard(
                std::process::Command::new("Xvfb")
                    .arg(":8") // TODO don't assume :8 isn't being used
                    .arg("-screen")
                    .arg(",0")
                    .arg("1024x768x16")
                    .spawn()
                    .unwrap(),
            ))
        } else {
            None
        };

        let mut factorio = ChildGuard(
            factorio_cmd
                .env("LD_PRELOAD", &setup_guard.lib_path)
                .env("FBRS_OUTPUT", output)
                .arg("--disable-audio")
                .arg("--disable-migration-window")
                // --benchmark-graphics unpauses the game, but swollows errors
                // --load-game is to figure out why something broke
                .arg(if debug {
                    "--load-game"
                } else {
                    "--benchmark-graphics"
                })
                .arg(map)
                //.stdout(std::process::Stdio::null()) // TODO scan output for errors?
                .spawn()
                .unwrap(),
        );

        let (tx, rx) = crossbeam::channel::unbounded::<()>();

        let ctrlc_tx = tx.clone();
        ctrlc::set_handler(move || {
            ctrlc_tx.send(()).unwrap();
        })
        .unwrap();

        std::thread::spawn(move || {
            factorio.wait().unwrap();
            tx.send(()).unwrap();
        });

        rx.recv().unwrap()
    })
    .unwrap();
}
