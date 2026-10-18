#![expect(clippy::cast_ptr_alignment)]

use anstyle::AnsiColor;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use hexdump::{hexdump_iter, sanitize_byte};
use nut::{Bucket, BucketStats, DB, DBBuilder, FreedOrPageInfo, TxGuard};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

const HOMEPAGE: &str = env!("CARGO_PKG_HOMEPAGE");
const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

fn validate_path(p: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(p);

    if path.exists() {
        Ok(path)
    } else {
        Err("Path not exists".to_string())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Dump {
            id,
            length,
            path_shim,
        } => dump(DumpOptions {
            path: path_shim.path,
            id,
            len: length,
        }),
        Commands::Info { check, path_shim } => info(InfoOptions {
            path: path_shim.path,
            check,
        }),
        Commands::Pages { ids, path_shim } => pages(PagesOptions {
            path: path_shim.path,
            ids,
        }),
        Commands::Tree { path_shim } => tree(TreeOptions {
            path: path_shim.path,
        }),
        Commands::Check { path_shim } => check(CheckOptions {
            path: path_shim.path,
        }),
    }
}

#[derive(Debug, Parser)]
#[command(about="Nut Database CLI", author, version, long_about =  format!(
"{}

homepage:
{}",
DESCRIPTION, HOMEPAGE,
))]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Dumps hex of the page
    Dump {
        /// id of the page
        #[arg(short, long)]
        id: usize,

        /// print no more than provided length
        #[arg(short, long)]
        length: Option<u64>,

        #[command(flatten)]
        path_shim: PathShim,
    },
    /// Prints database info
    Info {
        /// run db check
        #[arg(short, long)]
        check: bool,

        #[command(flatten)]
        path_shim: PathShim,
    },
    /// Prints a table of pages with their type (Meta, Leaf, Branch, Freelist)
    ///
    /// Leaf and branch pages will show a key count in the "items" column while the
    /// freelist will show the number of free pages in the "items" column.
    /// The "overflow" column shows the number of blocks that the page spills over
    /// into. Normally there is no overflow but large keys and values can cause
    ///a single page to take up multiple blocks.
    Pages {
        /// if provided, info will be given only on requested ids. takes multiple values
        #[arg(short, long = "id")]
        ids: Option<Vec<usize>>,

        #[command(flatten)]
        path_shim: PathShim,
    },
    /// Prints buckets tree
    Tree {
        #[command(flatten)]
        path_shim: PathShim,
    },
    /// Runs an exhaustive check to verify that all pages are accessible or are marked as freed
    Check {
        #[command(flatten)]
        path_shim: PathShim,
    },
}
#[derive(Debug, Args)]
struct PathShim {
    /// path to database
    #[arg(short, long, value_parser=validate_path)]
    path: PathBuf,
}

#[derive(Debug)]
struct DumpOptions {
    path: PathBuf,
    id: usize,
    len: Option<u64>,
}

fn dump(o: DumpOptions) -> Result<()> {
    let mut file = std::fs::File::open(&o.path).context("Can't open file")?;
    let page_size = DB::get_meta(&mut file)
        .map_err(anyhow::Error::msg)?
        .page_size;
    let offset = u64::from(page_size) * o.id as u64;
    let meta = file.metadata().context("Can't get file meta info")?.len();
    if offset > meta {
        bail!("ID out of bounds");
    };
    let mut overflowbuf = [0u8; 4];

    file.seek(SeekFrom::Start(offset + 12))
        .context("Can't seek file")?;
    file.read_exact(&mut overflowbuf)
        .context("Can't read file")?;
    let overflow = unsafe { *(&raw const overflowbuf[0]).cast::<u32>() };
    let mut take = (u64::from(overflow) + 1) * u64::from(page_size);
    if let Some(len) = o.len {
        take = u64::min(take, len);
    };

    file.seek(SeekFrom::Start(offset))
        .context("Can't seek file")?;
    let mut bound = Read::by_ref(&mut file).take(take);

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut obuf = [0u8; 1024];
    let newline = [0x0a];
    let mut written = 0;
    while let Ok(size) = bound.read(&mut obuf[written..]) {
        if size == 0 {
            if written > 0 {
                for line in hexdump_iter(&obuf[..written]) {
                    stdout
                        .write_all(line.as_bytes())
                        .context("Can't print output")?;
                    stdout.write_all(&newline).context("Can't print output")?;
                }
            }
            break;
        }
        written += size;
        if written == 1024 {
            for line in hexdump_iter(&obuf) {
                stdout
                    .write_all(line.as_bytes())
                    .context("Can't print output")?;
                stdout.write_all(&newline).context("Can't print output")?;
            }
        }
    }
    stdout.write(&[0x0a]).context("Can't print output")?;
    Ok(())
}

struct CheckOptions {
    path: PathBuf,
}

fn check(o: CheckOptions) -> Result<()> {
    let green = AnsiColor::Green.on_default();
    let red = AnsiColor::Red.on_default();

    let db = DBBuilder::new(o.path).read_only(true).build()?;
    let tx = db.begin_tx()?;
    let receiver = tx.check();
    let mut errlen = 0;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    for err in receiver {
        writeln!(stdout, "    {err}").context("Can't write output")?;
        errlen += 1;
    }
    if errlen != 0 {
        bail!("{red}Check failed{red:#}");
    }
    writeln!(stdout, "{green}Everything ok{green:#}").context("Can't write output")?;
    Ok(())
}

struct InfoOptions {
    path: PathBuf,
    check: bool,
}

fn info(o: InfoOptions) -> Result<()> {
    {
        let db = DBBuilder::new(&o.path).read_only(true).build()?;
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        writeln!(&mut stdout, "Page size: {}", db.page_size()).context("Can't write output")?;
        writeln!(&mut stdout, "Freelist id: {}", db.meta()?.freelist)
            .context("Can't write output")?;

        {
            let tx = db.begin_tx()?;
            let mut stats = BucketStats {
                ..Default::default()
            };
            let mut count = 0;
            let mut bucket_keys = vec![];
            tx.for_each(Box::new(|key, bucket| -> Result<(), String> {
                stats += bucket.ok_or("Not a bucket")?.stats();
                bucket_keys.push(key.to_vec());
                count += 1;
                Ok(())
            }))?;
            writeln!(&mut stdout, "\nBucket keys ({count}):").context("Can't write output")?;
            for key in bucket_keys {
                let sanitized: String = key.clone().into_iter().map(sanitize_byte).collect();
                writeln!(&mut stdout, "    {sanitized:<20} {key:02X?}")
                    .context("Can't write output")?;
            }
            writeln!(&mut stdout, "\n{stats:#?}").context("Can't write output")?;
        }

        if o.check {
            writeln!(&mut stdout, "\nCheck:").context("Can't write output")?;
        }
    }

    if o.check {
        check(CheckOptions { path: o.path })?;
    }

    Ok(())
}

struct PagesOptions {
    path: PathBuf,
    ids: Option<Vec<usize>>,
}

fn pages(o: PagesOptions) -> Result<()> {
    let green = AnsiColor::Green.on_default();
    let db = DBBuilder::new(&o.path).read_only(true).build()?;
    let tx = db.begin_tx()?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    let paddings = (6, 15, 7, 8);

    writeln!(
        &mut stdout,
        "{green}{:>width1$} {:<width2$} {:>width3$} {:>width4$}{green:#}",
        "ID",
        "Type",
        "Count",
        "Overflow",
        width1 = paddings.0,
        width2 = paddings.1,
        width3 = paddings.2,
        width4 = paddings.3
    )
    .context("Can't write output")?;
    writeln!(
        &mut stdout,
        "{} {} {} {}",
        "-".repeat(paddings.0),
        "-".repeat(paddings.1),
        "-".repeat(paddings.2),
        "-".repeat(paddings.3)
    )
    .context("Can't write output")?;

    let freed = tx.freed()?;

    let ids = match o.ids {
        Some(ids) => ids,
        None => get_page_ids(&tx)?,
    };

    let mut overflowed = HashSet::new();
    let pages_info = ids
        .into_iter()
        .map(|id| -> Result<_> {
            if freed.contains_key(&(id as u64)) {
                return Ok(Some(FreedOrPageInfo::Freed(id)));
            }
            if overflowed.contains(&id) {
                return Ok(None);
            }
            let info = tx
                .page_info(id)?
                .with_context(|| format!("Page {id} not found"))?;
            if info.overflow_count > 0 {
                if tx.page_info(id + info.overflow_count)?.is_none() {
                    bail!("Page {id}: overflow out of bounds");
                }
                for i in 1..=info.overflow_count {
                    overflowed.insert(id + i);
                }
            }
            Ok(Some(FreedOrPageInfo::PageInfo(info)))
        })
        .flat_map(|p| match p {
            Ok(Some(p)) => vec![Ok(p)],
            Ok(None) => vec![],
            Err(e) => vec![Err(e)],
        })
        .collect::<Result<Vec<_>, _>>()?;

    for info in pages_info {
        match info {
            FreedOrPageInfo::Freed(id) => {
                writeln!(&mut stdout, "{id:>6} free").context("Can't write output")?
            }
            FreedOrPageInfo::PageInfo(p) => writeln!(
                &mut stdout,
                "{:>width1$} {:<width2$} {:>width3$} {:>width4$}",
                p.id,
                format!("{:?}", p.ptype),
                p.count,
                p.overflow_count,
                width1 = paddings.0,
                width2 = paddings.1,
                width3 = paddings.2,
                width4 = paddings.3,
            )
            .context("Can't write output")?,
        }
    }
    Ok(())
}

fn get_page_ids(tx: &TxGuard) -> Result<Vec<usize>> {
    let mut ids = vec![];
    let mut id = 0;
    while let Some(_p) = tx.page_info(id)? {
        ids.push(id);
        id += 1;
    }
    Ok(ids)
}

struct TreeOptions {
    path: PathBuf,
}

fn tree_writer(
    indent_level: usize,
    mut out: &mut std::io::StdoutLock<'_>,
    key: &[u8],
    bucket: Option<&Bucket>,
) -> Result<(), String> {
    if let Some(ubucket) = bucket {
        let sanitized: String = key.iter().copied().map(sanitize_byte).collect();
        writeln!(
            &mut out,
            "{:<40} {:02X?}",
            format!("{}{}", " ".repeat(indent_level * 2), sanitized),
            key
        )
        .map_err(|_| "Can't write output")?;

        let buckets = ubucket.buckets();

        for b_name in buckets {
            tree_writer(indent_level + 1, out, &b_name, ubucket.bucket(&b_name))?;
        }
    };

    Ok(())
}

fn tree(o: TreeOptions) -> Result<()> {
    let db = DBBuilder::new(o.path).read_only(true).build()?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    writeln!(&mut stdout, "{:<40} binary", "key").context("Can't write output")?;
    writeln!(&mut stdout, "{} {}", "=".repeat(40), "=".repeat(20)).context("Can't write output")?;

    {
        let tx = db.begin_tx()?;
        tx.for_each(Box::new(
            |key: &[u8], bucket: Option<&Bucket>| -> Result<(), String> {
                tree_writer(0, &mut stdout, key, bucket)?;

                Ok(())
            },
        ))?;
    }

    Ok(())
}
