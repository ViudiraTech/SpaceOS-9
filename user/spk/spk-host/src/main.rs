//
//       user/spk/spk-host/src/main.rs
//       spk-host command line interface
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::path::PathBuf;
use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use clap::{Parser, Subcommand};
use serde_json::json;
use spk_core::keys::{
    load_secret_key, sign_file, signature_path, write_keypair, PublicKey, DEFAULT_COMMENT,
};
use spk_core::Error;
use spk_host::{generate, pack, verify_index, verify_repo};

#[derive(Parser)]
#[command(
    name = "spk-host",
    version,
    about = "SpaceOS-9 package builder and repository signer"
)]
struct Cli {
    /// Machine-readable output
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an ed25519 signing key pair
    Keygen {
        #[arg(long)]
        name: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = DEFAULT_COMMENT)]
        comment: String,
        #[arg(long)]
        force: bool,
    },
    /// Print the key id of a key file
    Keyid {
        #[arg(long)]
        key: PathBuf,
    },
    /// Build a .spk from a manifest and a staging tree
    Pack {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value_t = 19)]
        level: i32,
    },
    /// Scan a repository directory and write index.json (and sign it)
    Index {
        #[arg(long)]
        repo_dir: PathBuf,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        arch: String,
        /// Private key to sign with, or public key with --no-sign
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        generated_at: Option<u64>,
        #[arg(long)]
        no_sign: bool,
    },
    /// Create a detached .sig for an arbitrary file
    Sign {
        file: PathBuf,
        #[arg(long)]
        key: PathBuf,
    },
    /// Verify an index signature, or a whole repository with --repo-dir
    Verify {
        /// index.json to verify (omit when using --repo-dir)
        index: Option<PathBuf>,
        #[arg(long)]
        sig: Option<PathBuf>,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        repo_dir: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(exit_code(&error))
        }
    }
}

fn exit_code(error: &Error) -> u8 {
    match error {
        Error::Signature(_) => 3,
        _ => 1,
    }
}

fn run(cli: Cli) -> Result<ExitCode, Error> {
    match cli.command {
        Command::Keygen {
            name,
            out,
            comment,
            force,
        } => {
            let (secret, public, key) = write_keypair(&out, &name, &comment, force)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "key_id": key.key_id,
                        "comment": key.comment,
                        "private_key": secret,
                        "public_key": public,
                        "public_key_base64": BASE64.encode(key.public().bytes),
                    }))?
                );
            } else {
                println!(":: Generating ed25519 key {}", key.key_id);
                println!("   private: {} (0600)", secret.display());
                println!("   public:  {} (0644)", public.display());
            }
        }
        Command::Keyid { key } => {
            let public = PublicKey::from_file(&key)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "key_id": public.key_id,
                        "comment": public.comment,
                        "public_key": key,
                        "public_key_base64": BASE64.encode(public.bytes),
                    }))?
                );
            } else {
                println!("{}", public.key_id);
            }
        }
        Command::Pack {
            manifest,
            root,
            output,
            level,
        } => {
            let result = pack(&manifest, &root, &output, level)?;
            for warning in &result.warnings {
                eprintln!("warning: {warning}");
            }
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "file": result.output,
                        "filename": result.filename,
                        "sha256": result.sha256,
                        "size": result.size,
                        "name": result.manifest.name,
                        "version": result.manifest.version().to_string(),
                        "arch": result.manifest.arch,
                    }))?
                );
            } else {
                println!(":: Packing {}", result.filename);
                println!("   sha256: {}", result.sha256);
                println!("   size:   {} bytes", result.size);
            }
        }
        Command::Index {
            repo_dir,
            repo,
            arch,
            key,
            generated_at,
            no_sign,
        } => {
            let result = generate(&repo_dir, &repo, &arch, &key, generated_at, !no_sign)?;
            for warning in &result.warnings {
                eprintln!("warning: {warning}");
            }
            if cli.json {
                let packages: Vec<_> = result
                    .index
                    .packages
                    .iter()
                    .map(|package| {
                        json!({
                            "name": package.name,
                            "version": package.version().to_string(),
                            "arch": package.arch,
                            "filename": package.filename,
                            "sha256": package.sha256,
                            "size": package.size,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "index": result.index_path,
                        "repo": result.index.repo,
                        "arch": result.index.arch,
                        "generated_at": result.index.generated_at,
                        "key_id": result.index.key_id,
                        "signed": result.signed,
                        "packages": packages,
                    }))?
                );
            } else {
                println!(":: Indexing {} ({})", result.index.repo, result.index.arch);
                for package in &result.index.packages {
                    println!("   {} {}", package.name, package.version());
                }
                if result.signed {
                    println!(":: Signing index.json ({})", result.index.key_id);
                } else {
                    println!("warning: index.json written without a signature");
                }
            }
        }
        Command::Sign { file, key } => {
            let key = load_secret_key(&key)?;
            let signature = sign_file(&key, &file)?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "file": file,
                        "signature": signature_path(&file),
                        "key_id": signature.key_id,
                        "sha256": signature.sha256,
                    }))?
                );
            } else {
                println!(":: Signing {} ({})", file.display(), key.key_id);
                println!("   {}", signature_path(&file).display());
            }
        }
        Command::Verify {
            index,
            sig,
            key,
            repo_dir,
        } => match (index, repo_dir) {
            (Some(_), Some(_)) => {
                eprintln!("error: specify either an index file or --repo-dir, not both");
                return Ok(ExitCode::from(2));
            }
            (None, None) => {
                eprintln!("error: specify an index file or --repo-dir");
                return Ok(ExitCode::from(2));
            }
            (None, Some(repo_dir)) => {
                let report = verify_repo(&repo_dir, &key)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "repo": report.index.repo,
                            "arch": report.index.arch,
                            "key_id": report.index.key_id,
                            "packages": report.packages,
                            "status": "ok",
                        }))?
                    );
                } else {
                    println!(":: Verifying {} ({})", report.index.repo, report.index.arch);
                    for package in &report.packages {
                        println!("   {package} ... OK");
                    }
                    println!("index.json: OK");
                }
            }
            (Some(index), None) => {
                let parsed = verify_index(&index, sig.as_deref(), &key)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "repo": parsed.repo,
                            "arch": parsed.arch,
                            "key_id": parsed.key_id,
                            "packages": parsed.packages.len(),
                            "status": "ok",
                        }))?
                    );
                } else {
                    println!(":: Verifying {} ({})", parsed.repo, parsed.arch);
                    println!("   package(s): {}", parsed.packages.len());
                    println!("index.json: OK");
                }
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}
