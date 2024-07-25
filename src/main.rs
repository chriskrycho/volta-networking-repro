use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
};

use attohttpc::header::{HeaderMap, HeaderName};
use flate2::read::GzDecoder;
use headers::{AcceptRanges, ContentLength, Header, HeaderMapExt, Range};
use log::trace;
use simplelog::{ColorChoice, Config, LevelFilter, TermLogger, TerminalMode};
use tee::TeeReader;

fn main() -> Result<(), Error> {
    configure_logger();
    let (url, out_dir) = args()?;
    trace!("Testing against URL: '{url}'");

    let output_path = url
        .split('/')
        .last()
        .map(|file_name| out_dir.join(file_name))
        .ok_or_else(|| Error::Usage {
            message: format!("Could not construct file name from URL: {url}"),
        })?;

    println!("Output file path: {}", output_path.display());

    let (status, headers, response) = attohttpc::get(&url).send()?.split();

    trace!("status: {status}");
    if !status.is_success() {
        return Err(Error::Http { status });
    }

    trace!("returned headers: {headers:?}");

    let compressed_size = content_length(&headers)?;
    trace!("Compressed size: {compressed_size}");

    let accepts_ranges = accepts_byte_ranges(&headers);
    trace!("Accepts byte ranges: {accepts_ranges}");

    let uncompressed_size = fetch_uncompressed_size(&url, compressed_size).unwrap();

    let file = File::create(&output_path)?;
    let data = Box::new(TeeReader::new(response, file));
    let decoded = GzDecoder::new(data);

    let mut acc = 0u64;
    let mut curr_per = 0f64;
    let mut tarball = tar::Archive::new(ProgressRead::new(decoded, (), |_, read| {
        // inelegant but usefully minimal.
        acc += read as u64;
        let percent_completed = 100.0 * (acc as f64 / uncompressed_size as f64);
        if percent_completed > curr_per + 1.0 {
            curr_per = percent_completed;
            trace!(
                "read {acc} / {uncompressed_size} bytes, (~{}%)",
                percent_completed as u64
            );
        }
    }));

    let out = output_path.with_file_name(output_path.to_str().unwrap().replace(".tar.gz", ""));
    tarball.unpack(out)?;

    Ok(())
}

fn configure_logger() {
    TermLogger::init(
        LevelFilter::Trace,
        Config::default(),
        TerminalMode::default(),
        ColorChoice::default(),
    )
    .expect("Set up the logger");
}

fn args() -> Result<(String, PathBuf), Error> {
    let mut args = std::env::args();
    let url = args.nth(1).ok_or_else(|| Error::Usage {
        message: "Provide the URL to download.".into(),
    })?;

    let out_dir = args
        .next()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .ok_or_else(|| Error::Usage {
            message: "Provide a directory to place the downloaded file in".into(),
        })?;

    Ok((url, out_dir))
}

fn accepts_byte_ranges(headers: &HeaderMap) -> bool {
    headers
        .typed_get::<AcceptRanges>()
        .is_some_and(|v| v == AcceptRanges::bytes())
}

/// Determines the length of an HTTP response's content in bytes, using
/// the HTTP `"Content-Length"` header.
fn content_length(headers: &HeaderMap) -> Result<u64, Error> {
    headers
        .typed_get::<ContentLength>()
        .map(|v| v.0)
        .ok_or_else(|| Error::MissingHeader {
            name: ContentLength::name().to_owned(),
        })
}

/// Determines the uncompressed size of a gzip file hosted at the specified
/// URL by fetching just the metadata associated with the file. This makes
/// an extra round-trip to the server, so it's only more efficient than just
/// downloading the file if the file is large enough that downloading it is
/// slower than the extra round trips.
fn fetch_uncompressed_size(url: &str, len: u64) -> Option<u64> {
    // if there is an error, we ignore it and return None, instead of failing
    fetch_isize(url, len)
        .map(|s| u32::from_le_bytes(s) as u64)
        .ok()
}

// From http://www.gzip.org/zlib/rfc-gzip.html#member-format
//
//   0   1   2   3   4   5   6   7
// +---+---+---+---+---+---+---+---+
// |     CRC32     |     ISIZE     |
// +---+---+---+---+---+---+---+---+
//
// ISIZE (Input SIZE)
//    This contains the size of the original (uncompressed) input data modulo 2^32.

/// Fetches just the `isize` field (the field that indicates the uncompressed size)
/// of a gzip file from a URL. This makes two round-trips to the server but avoids
/// downloading the entire gzip file. For very small files it's unlikely to be
/// more efficient than simply downloading the entire file up front.
fn fetch_isize(url: &str, len: u64) -> Result<[u8; 4], Error> {
    let (status, headers, mut response) = {
        let mut request = attohttpc::get(url);
        request
            .headers_mut()
            .typed_insert(Range::bytes(len - 4..len).unwrap());
        trace!("Requesting isize field. Request: {request:?}");
        request.send()?.split()
    };

    trace!("Uncompressed size (`isize`) status: {status}");

    if !status.is_success() {
        return Err(Error::Http { status });
    }

    let actual_length = content_length(&headers)?;

    if actual_length != 4 {
        return Err(Error::UnexpectedContentLength(actual_length));
    }

    let mut buf = [0; 4];
    response.read_exact(&mut buf)?;
    Ok(buf)
}

/// Loads the `isize` field (the field that indicates the uncompressed size)
/// of a gzip file from disk.
fn load_isize(file: &mut File) -> Result<[u8; 4], Error> {
    file.seek(SeekFrom::End(-4))?;
    let mut buf = [0; 4];
    file.read_exact(&mut buf)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(buf)
}

const USAGE: &str = "vnr <url> <output directory>";

#[derive(thiserror::Error)]
enum Error {
    #[error("{message}\nUsage: {USAGE}")]
    Usage { message: String },

    #[error("Network error: {source}")]
    Network {
        #[from]
        source: attohttpc::Error,
    },

    #[error("HTTP error: {status}")]
    Http { status: attohttpc::StatusCode },

    #[error("Missing header: {name}")]
    MissingHeader { name: HeaderName },

    #[error(transparent)]
    Io {
        #[from]
        source: io::Error,
    },

    #[error("Unexpected content length: {0}")]
    UnexpectedContentLength(u64),
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

/// A reader that reports incremental progress while reading.
pub struct ProgressRead<R: Read, T, F: FnMut(&T, usize) -> T> {
    source: R,
    accumulator: T,
    progress: F,
}

impl<R: Read, T, F: FnMut(&T, usize) -> T> Read for ProgressRead<R, T, F> {
    /// Read some bytes from the underlying reader into the specified buffer,
    /// and report progress to the progress callback. The progress callback is
    /// passed the current value of the accumulator as its first argument and
    /// the number of bytes read as its second argument. The result of the
    /// progress callback is stored as the updated value of the accumulator,
    /// to be passed to the next invocation of the callback.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.source.read(buf)?;
        let new_accumulator = {
            let progress = &mut self.progress;
            progress(&self.accumulator, len)
        };
        self.accumulator = new_accumulator;
        Ok(len)
    }
}

impl<R: Read, T, F: FnMut(&T, usize) -> T> ProgressRead<R, T, F> {
    /// Construct a new progress reader with the specified underlying reader,
    /// initial value for an accumulator, and progress callback.
    pub fn new(source: R, init: T, progress: F) -> ProgressRead<R, T, F> {
        ProgressRead {
            source,
            accumulator: init,
            progress,
        }
    }
}

impl<R: Read + Seek, T, F: FnMut(&T, usize) -> T> Seek for ProgressRead<R, T, F> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.source.seek(pos)
    }
}
