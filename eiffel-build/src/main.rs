//! Builds an AWS Nitro Enclave image (`.eif`) from two LinuxKit-style layout YAMLs.

#![forbid(unsafe_code)]

mod cpio;
mod layout;

use anyhow::{Context, Result, anyhow, bail};
use aws_nitro_enclaves_image_format::defs::EifIdentityInfo;
use aws_nitro_enclaves_image_format::generate_build_info;
use aws_nitro_enclaves_image_format::utils::EifBuilder;
use clap::Parser;
use sha2::{Digest, Sha256, Sha384};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(about, version)]
struct Args {
    /// Output name; writes <out-dir>/<NAME>.eif
    name: String,

    /// Customer layout YAML
    #[arg(long, default_value = "/workspace/layout.yml")]
    layout: PathBuf,

    /// Bootstrap layout YAML
    #[arg(long, default_value = "/eiffel/bootstrap-layout.yml")]
    bootstrap_layout: PathBuf,

    #[arg(long, default_value = "/blobs/bzImage")]
    kernel: PathBuf,

    #[arg(long, default_value = "/blobs/bzImage.config")]
    kernel_config: PathBuf,

    /// Single-line kernel command line; the trailing newline is stripped
    #[arg(long, default_value = "/blobs/cmdline")]
    cmdline_file: PathBuf,

    #[arg(long, default_value = "/out")]
    out_dir: PathBuf,

    /// EIF metadata img_version
    #[arg(long, default_value = "1.0")]
    image_version: String,

    /// Also write <NAME>-bootstrap.cpio.gz and <NAME>-customer.cpio.gz to out-dir
    #[arg(long)]
    keep_initrds: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let bootstrap_ramdisk = ramdisk_path(&args, "bootstrap");
    let customer_ramdisk = ramdisk_path(&args, "customer");
    let eif_tmp = args.out_dir.join(format!(".{}.eif.tmp", args.name));

    let result = build(&args, &bootstrap_ramdisk, &customer_ramdisk, &eif_tmp);

    // Leave no partial .eif behind, and no intermediate ramdisks unless asked for.
    let _ = std::fs::remove_file(&eif_tmp);
    if !args.keep_initrds {
        let _ = std::fs::remove_file(&bootstrap_ramdisk);
        let _ = std::fs::remove_file(&customer_ramdisk);
    }

    result
}

fn ramdisk_path(args: &Args, which: &str) -> PathBuf {
    let name = &args.name;
    args.out_dir.join(if args.keep_initrds {
        format!("{name}-{which}.cpio.gz")
    } else {
        format!(".{name}-{which}.cpio.gz.tmp")
    })
}

fn build(args: &Args, bootstrap: &Path, customer: &Path, eif_tmp: &Path) -> Result<()> {
    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating output directory {}", args.out_dir.display()))?;

    let cmdline = std::fs::read_to_string(&args.cmdline_file)
        .with_context(|| format!("reading {}", args.cmdline_file.display()))?
        .trim_end()
        .to_string();

    write_ramdisk(&args.bootstrap_layout, bootstrap)?;
    write_ramdisk(&args.layout, customer)?;

    // EifBuilder panics rather than returning on unreadable inputs, so check first.
    for path in [&args.kernel, &args.kernel_config] {
        if !path.is_file() {
            bail!("{} is not a readable file", path.display());
        }
    }
    let kernel_config = args
        .kernel_config
        .to_str()
        .ok_or_else(|| anyhow!("--kernel-config path is not valid UTF-8"))?;
    let build_info = generate_build_info!(kernel_config)
        .map_err(|e| anyhow!("reading kernel version from {kernel_config}: {e}"))?;

    let eif_info = EifIdentityInfo {
        img_name: args.name.clone(),
        img_version: args.image_version.clone(),
        build_info,
        docker_info: serde_json::Value::Null,
        custom_info: serde_json::Value::Null,
    };

    // No signing in this build: PCR8 stays zero and the KMS path is never entered.
    let mut builder = EifBuilder::new(&args.kernel, cmdline, None, Sha384::new(), 0, eif_info);
    // Order is a correctness requirement: the first ramdisk is measured into PCR1,
    // the rest into PCR2.
    builder.add_ramdisk(bootstrap);
    builder.add_ramdisk(customer);

    let mut file =
        File::create(eif_tmp).with_context(|| format!("creating {}", eif_tmp.display()))?;
    let pcrs = builder.write_to(&mut file);
    file.sync_all()
        .with_context(|| format!("writing {}", eif_tmp.display()))?;
    drop(file);

    let eif = args.out_dir.join(format!("{}.eif", args.name));
    std::fs::rename(eif_tmp, &eif)
        .with_context(|| format!("renaming {} to {}", eif_tmp.display(), eif.display()))?;

    write_checksum(&eif, &args.out_dir, &args.name)?;
    write_pcrs(pcrs, &args.out_dir, &args.name)?;
    Ok(())
}

fn write_ramdisk(layout_path: &Path, out: &Path) -> Result<()> {
    let items = layout::load(layout_path)?;
    let file = File::create(out).with_context(|| format!("creating {}", out.display()))?;
    cpio::write_gzipped(file, &items).with_context(|| format!("writing ramdisk {}", out.display()))
}

fn write_checksum(eif: &Path, out_dir: &Path, name: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    let mut file = File::open(eif).with_context(|| format!("reading {}", eif.display()))?;
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("reading {}", eif.display()))?;
    let line = format!("{}  {name}.eif\n", hex::encode(hasher.finalize()));

    let path = out_dir.join(format!("{name}.eif.sha256sum"));
    std::fs::write(&path, &line).with_context(|| format!("writing {}", path.display()))?;

    print!("SHA256 checksum:\n{line}");
    std::io::stdout().flush()?;
    Ok(())
}

fn write_pcrs(pcrs: BTreeMap<String, String>, out_dir: &Path, name: &str) -> Result<()> {
    // Drop the HashAlgorithm entry: its value is a debug dump of the hasher state and
    // the algorithm is fixed to SHA-384 anyway.
    let pcrs: BTreeMap<_, _> = pcrs
        .into_iter()
        .filter(|(k, _)| k.starts_with("PCR"))
        .collect();
    let json = serde_json::to_string_pretty(&pcrs)? + "\n";

    let path = out_dir.join(format!("{name}.pcrs.json"));
    std::fs::write(&path, &json).with_context(|| format!("writing {}", path.display()))?;

    print!("PCRs:\n{json}");
    std::io::stdout().flush()?;
    Ok(())
}
