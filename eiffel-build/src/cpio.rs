//! newc (SVR4, `070701`) cpio writer, gzipped.
//!
//! Everything that could vary between builds is pinned: uid/gid 0, mtime 0, inodes
//! numbered from 1 in emission order, no device numbers, and a gzip header with no
//! timestamp or filename. Two runs over the same layout produce identical bytes.

use crate::layout::{Body, Item};
use anyhow::{Context, Result, bail};
use flate2::{Compression, GzBuilder};
use std::fs::File;
use std::io::{self, Write};

const MAGIC: &[u8; 6] = b"070701";
const TRAILER: &str = "TRAILER!!!";
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

/// Write `items` as a gzipped newc archive.
pub fn write_gzipped<W: Write>(out: W, items: &[Item]) -> Result<()> {
    let mut encoder = GzBuilder::new().mtime(0).write(out, Compression::best());
    write_archive(&mut encoder, items)?;
    encoder.finish()?.flush()?;
    Ok(())
}

fn write_archive<W: Write>(out: &mut W, items: &[Item]) -> Result<()> {
    for (index, item) in items.iter().enumerate() {
        let ino = index as u64 + 1;
        match &item.body {
            Body::Directory => {
                header(out, &item.path, ino, S_IFDIR | item.mode, 2, 0)?;
            }
            Body::Inline(bytes) => {
                header(
                    out,
                    &item.path,
                    ino,
                    S_IFREG | item.mode,
                    1,
                    bytes.len() as u64,
                )?;
                out.write_all(bytes)?;
                pad(out, bytes.len() as u64)?;
            }
            Body::Source(source) => {
                let size = std::fs::metadata(source)
                    .with_context(|| format!("reading source {}", source.display()))?
                    .len();
                header(out, &item.path, ino, S_IFREG | item.mode, 1, size)?;
                let mut file = File::open(source)
                    .with_context(|| format!("opening source {}", source.display()))?;
                let copied = io::copy(&mut file, out)
                    .with_context(|| format!("copying source {}", source.display()))?;
                if copied != size {
                    bail!(
                        "source {} changed size while being archived",
                        source.display()
                    );
                }
                pad(out, size)?;
            }
        }
    }
    header(out, TRAILER, 0, 0, 1, 0)
}

fn header<W: Write>(
    out: &mut W,
    name: &str,
    ino: u64,
    mode: u32,
    nlink: u64,
    filesize: u64,
) -> Result<()> {
    let namesize = name.len() as u64 + 1;
    // ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor,
    // rdevmajor, rdevminor, namesize, check -- 8 uppercase hex chars each.
    let fields = [
        ino,
        u64::from(mode),
        0,
        0,
        nlink,
        0,
        filesize,
        0,
        0,
        0,
        0,
        namesize,
        0,
    ];
    let mut buf = Vec::with_capacity(110 + namesize as usize + 3);
    buf.extend_from_slice(MAGIC);
    for field in fields {
        if field > u64::from(u32::MAX) {
            bail!("{name}: value {field} does not fit a newc header field");
        }
        write!(buf, "{field:08X}")?;
    }
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    buf.resize(buf.len().next_multiple_of(4), 0);
    out.write_all(&buf)?;
    Ok(())
}

fn pad<W: Write>(out: &mut W, written: u64) -> Result<()> {
    let padding = (written.next_multiple_of(4) - written) as usize;
    out.write_all(&[0u8; 3][..padding])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn archive(items: &[Item]) -> Vec<u8> {
        let mut out = Vec::new();
        write_archive(&mut out, items).unwrap();
        out
    }

    fn sample() -> Vec<Item> {
        vec![
            Item {
                path: "file".into(),
                mode: 0o644,
                body: Body::Inline(b"hi\n".to_vec()),
            },
            Item {
                path: "dir".into(),
                mode: 0o755,
                body: Body::Directory,
            },
        ]
    }

    /// Golden archive for `sample()`, taken verbatim from GNU cpio 2.15:
    ///
    ///     echo hi > file && mkdir dir
    ///     printf 'file\ndir\n' | cpio -o -H newc --reproducible
    ///
    /// with two documented deviations applied to its output: mtime zeroed (GNU
    /// cpio keeps the real one) and inodes renumbered from 1 (GNU numbers from 0).
    /// GNU's trailing pad to a 512-byte block is dropped; initramfs does not need it.
    const GOLDEN: &str = concat!(
        "3037303730313030303030303031303030303831413430303030303030303030",
        "3030303030303030303030303031303030303030303030303030303030333030",
        "3030303030303030303030303030303030303030303030303030303030303030",
        "303030303035303030303030303066696C65000068690A003037303730313030",
        "3030303030323030303034314544303030303030303030303030303030303030",
        "3030303030323030303030303030303030303030303030303030303030303030",
        "3030303030303030303030303030303030303030303030303030303030343030",
        "3030303030306469720000003037303730313030303030303030303030303030",
        "3030303030303030303030303030303030303030303030303031303030303030",
        "3030303030303030303030303030303030303030303030303030303030303030",
        "3030303030303030303030303030303030423030303030303030545241494C45",
        "5221212100000000",
    );

    #[test]
    fn header_layout_matches_gnu_cpio() {
        assert_eq!(hex::encode_upper(archive(&sample())), GOLDEN);
    }

    #[test]
    fn gzip_output_is_byte_for_byte_deterministic() {
        let first = {
            let mut out = Vec::new();
            write_gzipped(&mut out, &sample()).unwrap();
            out
        };
        let second = {
            let mut out = Vec::new();
            write_gzipped(&mut out, &sample()).unwrap();
            out
        };
        assert_eq!(first, second);

        let mut inflated = Vec::new();
        GzDecoder::new(&first[..])
            .read_to_end(&mut inflated)
            .unwrap();
        assert_eq!(inflated, archive(&sample()));
    }

    #[test]
    fn pads_names_and_data_to_four_bytes() {
        // "abc" -> 110 + 4 = 114, padded to 116; 5 bytes of data padded to 8.
        let out = archive(&[Item {
            path: "abc".into(),
            mode: 0o600,
            body: Body::Inline(b"12345".to_vec()),
        }]);
        assert_eq!(&out[110..116], b"abc\0\0\0");
        assert_eq!(&out[116..124], b"12345\0\0\0");
        assert_eq!(&out[124..130], MAGIC);
    }
}
