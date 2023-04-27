//! Module for `.torrent` files ([v1](http://bittorrent.org/beps/bep_0003.html))
//! related parsing/encoding/creation.

use crate::bencode::BencodeElem;
use crate::LavaTorrentError;
use itertools::Itertools;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use sha1::{Digest, Sha1};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::thread::JoinHandle;

mod build;
mod read;
mod write;

const PIECE_STRING_LENGTH: usize = 20;

// The escaping rules for magnet URIs are not specified in BEP9,
// so we simply escape '&'. We do not escape space here, since
// percent_encoding escapes it to '%20' instead of '+'.
// Instead, we manually replace it with '+' later in the code.
// This means that we do have to escape actual '+'s though!
const MAGNET_COMPONENT: &AsciiSet = &CONTROLS.add(b'&').add(b'+');

/// Corresponds to a bencode dictionary.
pub type Dictionary = HashMap<String, BencodeElem>;
/// Corresponds to the `announce-list` in [BEP 12](http://bittorrent.org/beps/bep_0012.html).
pub type AnnounceList = Vec<Vec<String>>;
/// A piece in `pieces`--the SHA1 hash of a torrent block.
pub type Piece = Vec<u8>;
/// Corresponds to a bencode integer. The underlying type is `i64`.
/// Technically a bencode integer has no size limit, but it is not
/// so in the current implementation. By using a type alias it is
/// easier to change the underlying type in the future.
pub type Integer = i64;

/// A file contained in a torrent.
///
/// Modeled after the specifications
/// in [BEP 3](http://bittorrent.org/beps/bep_0003.html). Unknown/extension
/// fields will be placed in `extra_fields`. If you need
/// any of those extra fields you would have to parse it yourself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct File {
    /// File size in bytes.
    pub length: Integer,
    /// File path, relative to [`Torrent`](struct.Torrent.html)'s `name` field.
    pub path: PathBuf,
    /// Fields not defined in [BEP 3](http://bittorrent.org/beps/bep_0003.html).
    pub extra_fields: Option<Dictionary>,
}

/// Everything found in a *.torrent* file.
///
/// Modeled after the specifications
/// in [BEP 3](http://bittorrent.org/beps/bep_0003.html) and
/// [BEP 12](http://bittorrent.org/beps/bep_0012.html). Unknown/extension
/// fields will be placed in `extra_fields` (if the unknown
/// fields are found in the `info` dictionary then they are placed in
/// `extra_info_fields`). If you need any of those extra fields you would
/// have to parse it yourself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Torrent {
    /// URL of the torrent's tracker.
    pub announce: Option<String>,
    /// Announce list as defined in [BEP 12](http://bittorrent.org/beps/bep_0012.html).
    pub announce_list: Option<AnnounceList>,
    /// Total torrent size in bytes (i.e. sum of all files' sizes).
    pub length: Integer,
    /// If the torrent contains only 1 file then `files` is `None`.
    pub files: Option<Vec<File>>,
    /// If the torrent contains only 1 file then `name` is the file name.
    /// Otherwise it's the suggested root directory's name.
    pub name: String,
    /// Block size in bytes.
    pub piece_length: Integer,
    /// SHA1 hashes of each block.
    pub pieces: Vec<Piece>,
    /// Top-level fields not defined in [BEP 3](http://bittorrent.org/beps/bep_0003.html).
    pub extra_fields: Option<Dictionary>,
    /// Fields in `info` not defined in [BEP 3](http://bittorrent.org/beps/bep_0003.html).
    pub extra_info_fields: Option<Dictionary>,
}

/// Builder for creating `Torrent`s from files.
///
/// This struct is used for **creating** `Torrent`s, so that you can
/// encode/serialize them to *.torrent* files. If you want to read
/// existing *.torrent* files then use [`Torrent::read_from_file()`]
/// or [`Torrent::read_from_bytes()`].
///
/// Required fields: `path` and `piece_length`.
/// They are set when calling the constructor [`new()`].
///
/// Optional fields can be set by calling the corresponding methods
///  (e.g. [`set_announce()`]). Fields can be updated in the same way.
///
/// # Notes
///
/// ## Hidden Files
///
/// **\*nix hidden files/dirs are ignored.**
///
/// Reasoning:
/// when handling these special "files", there are many decisions to make:
/// - Should they be ignored, included, or selectively ignored/included?
/// - Should included/ignored entries be marked specially (e.g. [BEP 47])?
/// - Should users be allowed to configure the settings?
/// - If users can configure the settings, what would be the ideal defaults?
/// - ...
///
/// Apparently it's not easy to make these decisions.
/// Therefore these files are ignored for now.
/// Clients like Deluge and qBittorrent also ignore hidden entries.
///
/// ## Parallel Hashing
///
/// By default, pieces are hashed in parallel. The default level of
/// parallelism is equal to the number of physical cores. To adjust
/// the parallelism level or to force single-threaded hashing, use
/// [`set_num_threads()`]. Note that this setting is **specific to
/// each builder and not global**.
///
/// [`Torrent::read_from_file()`]: struct.Torrent.html#method.read_from_file
/// [`Torrent::read_from_bytes()`]: struct.Torrent.html#method.read_from_bytes
/// [`new()`]: #method.new
/// [`set_announce()`]: #method.set_announce
/// [BEP 47]: http://bittorrent.org/beps/bep_0047.html
/// [`set_num_threads()`]: #method.set_num_threads
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TorrentBuilder {
    announce: Option<String>,
    announce_list: Option<AnnounceList>,
    name: Option<String>,
    path: PathBuf,
    piece_length: Integer,
    extra_fields: Option<Dictionary>,
    extra_info_fields: Option<Dictionary>,
    is_private: bool,
    num_threads: usize,
}

/// Handle for non-blocking torrent builds.
///
/// See [`TorrentBuilder::build_non_blocking()`] for an example.
///
/// [`TorrentBuilder::build_non_blocking()`]: struct.TorrentBuilder.html#method.build_non_blocking
#[derive(Debug)]
pub struct TorrentBuild {
    n_piece_processed: Arc<AtomicU64>,
    n_piece_total: Arc<AtomicU64>,
    is_canceled: Arc<AtomicBool>,
    builder_thread: JoinHandle<Result<Torrent, LavaTorrentError>>,
}

#[derive(Clone, Debug)]
struct TorrentBuildInternal {
    n_piece_processed: Arc<AtomicU64>,
    n_piece_total: Arc<AtomicU64>,
    is_canceled: Arc<AtomicBool>,
}

impl File {
    /// Construct the `File`'s absolute path using `parent`.
    ///
    /// Caller has to ensure that `parent` is an absolute path.
    /// Otherwise an error would be returned.
    ///
    /// This method effectively appends/joins `self.path` to `parent`.
    pub fn absolute_path<P>(&self, parent: P) -> Result<PathBuf, LavaTorrentError>
    where
        P: AsRef<Path>,
    {
        let result = parent.as_ref().join(&self.path);
        if result.is_absolute() {
            Ok(result)
        } else {
            Err(LavaTorrentError::InvalidArgument(Cow::Borrowed(
                "Joined path is not absolute.",
            )))
        }
    }
}

impl Torrent {
    /// Construct the `info` dict based on the fields of `self`.
    ///
    /// Certain operations on torrents, such as calculating info
    /// hashes, require the extracted `info` dict. This
    /// convenience method does that.
    ///
    /// Note that the `info` dict
    /// is constructed each time this method is called (i.e.
    /// the return value is not cached). If caching is needed
    /// then the caller should handle that.
    ///
    /// Since `self` is taken by reference, and the result is
    /// returned by value, certain values will be cloned. Please
    /// be aware of this overhead.
    pub fn construct_info(&self) -> BencodeElem {
        let mut info: HashMap<String, BencodeElem> = HashMap::new();

        if let Some(ref files) = self.files {
            info.insert(
                "files".to_owned(),
                BencodeElem::List(
                    files
                        .clone()
                        .into_iter()
                        .map(|file| file.into_bencode_elem())
                        .collect(),
                ),
            );
        } else {
            info.insert("length".to_owned(), BencodeElem::Integer(self.length));
        }

        info.insert("name".to_owned(), BencodeElem::String(self.name.clone()));
        info.insert(
            "piece length".to_owned(),
            BencodeElem::Integer(self.piece_length),
        );
        info.insert(
            "pieces".to_owned(),
            BencodeElem::Bytes(self.pieces.clone().into_iter().flatten().collect()),
        );

        if let Some(ref extra_info_fields) = self.extra_info_fields {
            info.extend(extra_info_fields.clone());
        }

        BencodeElem::Dictionary(info)
    }

    /// Calculate the `Torrent`'s info hash as defined in
    /// [BEP 3](http://bittorrent.org/beps/bep_0003.html).
    ///
    /// Note that the calculated info hash is not cached.
    /// So if this method is called multiple times, multiple
    /// calculations will be performed. To avoid that, the
    /// caller should cache the return value as needed.
    pub fn info_hash(&self) -> String {
        format!("{:x}", Sha1::digest(self.construct_info().encode()))
    }

    /// Calculate the `Torrent`'s info hash as defined in
    /// [BEP 3](http://bittorrent.org/beps/bep_0003.html).
    ///
    /// Note that the calculated info hash is not cached.
    /// So if this method is called multiple times, multiple
    /// calculations will be performed. To avoid that, the
    /// caller should cache the return value as needed.
    pub fn info_hash_bytes(&self) -> Vec<u8> {
        Sha1::digest(self.construct_info().encode()).to_vec()
    }

    /// Calculate the `Torrent`'s magnet link as defined in
    /// [BEP 9](http://bittorrent.org/beps/bep_0009.html).
    ///
    /// The `dn` parameter is set to `self.name`.
    ///
    /// Either `self.announce` or all trackers in `self.announce_list` will be used,
    /// meaning that there might be multiple `tr` entries. We don't use both because
    /// per [BEP 12](http://bittorrent.org/beps/bep_0012.html):
    /// "If the client is compatible with the multitracker specification, and if the
    /// `announce-list` key is present, the client will ignore the `announce` key
    /// and only use the URLs in `announce-list`."
    ///
    /// If neither `self.announce` nor `self.announce_list` is present, the output
    /// won't contain any `tr` parameter.
    ///
    /// The `x.pe` parameter (for peer addresses) is currently not supported.
    ///
    /// `self.extra_fields["url-list"]` will be used to construct `ws` parameters.
    /// It must be either a string or a list of strings.
    pub fn magnet_link(&self) -> Result<String, LavaTorrentError> {
        fn encode_component(from: &str) -> String {
            // percent_encoding escapes space as '%20', which is not accepted
            // by clients such as transmission, so we escape it manually to '+'.
            utf8_percent_encode(from, MAGNET_COMPONENT)
                .to_string()
                .replace(' ', "+")
        }

        let tr = if let Some(ref list) = self.announce_list {
            list.iter()
                .format_with("", |tier, f| {
                    f(&format_args!(
                        "{}",
                        tier.iter().format_with("", |url, f| f(&format_args!(
                            "&tr={}",
                            encode_component(url)
                        )))
                    ))
                })
                .to_string()
        } else if let Some(ref announce) = self.announce {
            format!("&tr={}", encode_component(announce))
        } else {
            String::new()
        };

        let ws = match self
            .extra_fields
            .as_ref()
            .and_then(|fields| fields.get("url-list"))
        {
            Some(BencodeElem::String(seed)) => Some(vec![seed]),
            Some(BencodeElem::List(ref seeds)) => Some(
                seeds
                    .iter()
                    .map(|elem| match elem {
                        BencodeElem::String(url) => Ok(url),
                        _ => {
                            return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                                r#""url-list" is a list but contains a non-string element."#,
                            )));
                        }
                    })
                    .collect::<Result<Vec<&String>, LavaTorrentError>>()?,
            ),
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""url-list" is neither a string nor a list."#,
                )));
            }
            None => None,
        };
        let ws = match ws {
            Some(ws) => ws
                .iter()
                .format_with("", |&url, f| {
                    f(&format_args!("&ws={}", encode_component(url)))
                })
                .to_string(),
            None => String::new(),
        };

        Ok(format!(
            "magnet:?xt=urn:btih:{}&dn={}{}{}",
            self.info_hash(),
            self.name,
            tr,
            ws,
        ))
    }

    /// Check if this torrent is private as defined in
    /// [BEP 27](http://bittorrent.org/beps/bep_0027.html).
    ///
    /// Returns `true` if `private` maps to a bencode integer `1`.
    /// Returns `false` otherwise.
    pub fn is_private(&self) -> bool {
        if let Some(ref dict) = self.extra_info_fields {
            match dict.get("private") {
                Some(&BencodeElem::Integer(val)) => val == 1,
                Some(_) => false,
                None => false,
            }
        } else {
            false
        }
    }
}

impl fmt::Display for File {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "{}\n\
             -size: {} bytes",
            self.path.as_path().display(),
            self.length
        )?;

        if let Some(ref fields) = self.extra_fields {
            write!(
                f,
                "{}",
                fields
                    .iter()
                    .sorted_by_key(|&(key, _)| key.as_bytes())
                    .format_with("", |(k, v), f| f(&format_args!("-{}: {}\n", k, v)))
            )?;
        }

        writeln!(f, "========================================")
    }
}

impl fmt::Display for Torrent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "{}.torrent", self.name)?;
        if let Some(ref announce) = self.announce {
            writeln!(f, "-announce: {}", announce)?;
        }
        if let Some(ref tiers) = self.announce_list {
            writeln!(
                f,
                "-announce-list: [{}]",
                tiers.iter().format_with(", ", |tier, f| f(&format_args!(
                    "[{}]",
                    itertools::join(tier, ", ")
                )))
            )?;
        }
        writeln!(f, "-size: {} bytes", self.length)?;
        writeln!(f, "-piece length: {} bytes", self.piece_length)?;

        if let Some(ref fields) = self.extra_fields {
            write!(
                f,
                "{}",
                fields
                    .iter()
                    .sorted_by_key(|&(key, _)| key.as_bytes())
                    .format_with("", |(k, v), f| f(&format_args!("-{}: {}\n", k, v)))
            )?;
        }

        if let Some(ref fields) = self.extra_info_fields {
            write!(
                f,
                "{}",
                fields
                    .iter()
                    .sorted_by_key(|&(key, _)| key.as_bytes())
                    .format_with("", |(k, v), f| f(&format_args!("-{}: {}\n", k, v)))
            )?;
        }

        if let Some(ref files) = self.files {
            writeln!(f, "-files:")?;
            for (counter, file) in files.iter().enumerate() {
                writeln!(f, "[{}] {}", counter + 1, file)?;
            }
        }

        writeln!(
            f,
            "-pieces: [{}]",
            self.pieces
                .iter()
                .format_with(", ", |piece, f| f(&format_args!(
                    "[{:02x}]",
                    piece.iter().format("")
                ))),
        )
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn absolute_path_ok() {
        let file = File {
            length: 42,
            path: PathBuf::from("dir1/file"),
            extra_fields: None,
        };

        assert_eq!(
            file.absolute_path("/root").unwrap(),
            PathBuf::from("/root/dir1/file")
        );
    }

    #[test]
    fn absolute_path_not_absolute() {
        let file = File {
            length: 42,
            path: PathBuf::from("dir1/file"),
            extra_fields: None,
        };

        match file.absolute_path("root") {
            Err(LavaTorrentError::InvalidArgument(m)) => {
                assert_eq!(m, "Joined path is not absolute.");
            }
            _ => panic!(),
        }
    }
}

#[cfg(test)]
mod torrent_tests {
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn construct_info_ok() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: Some(HashMap::from_iter(
                vec![("key".to_owned(), bencode_elem!("val"))].into_iter(),
            )),
        };

        assert_eq!(
            torrent.construct_info(),
            bencode_elem!({
                ("length", 4),
                ("name", "sample"),
                ("piece length", 2),
                ("pieces", (1, 2, 3, 4)),
                ("key", "val"),
            }),
        );
    }

    #[test]
    fn info_hash_ok() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.info_hash(),
            "074f42efaf8267f137f114f722d4e7d1dcbfbda5".to_owned(),
        );
    }

    #[test]
    fn magnet_link_ok() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.magnet_link().unwrap(),
            "magnet:?xt=urn:btih:074f42efaf8267f137f114f722d4e7d1dcbfbda5\
             &dn=sample&tr=url"
                .to_owned()
        );
    }

    #[test]
    fn magnet_link_with_announce_list() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: Some(vec![
                vec!["url1".to_owned()],
                vec!["url2".to_owned(), "url3".to_owned()],
            ]),
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.magnet_link().unwrap(),
            "magnet:?xt=urn:btih:074f42efaf8267f137f114f722d4e7d1dcbfbda5\
             &dn=sample&tr=url1&tr=url2&tr=url3"
                .to_owned()
        );
    }

    #[test]
    fn magnet_link_with_web_seed() {
        let torrent = Torrent {
            announce: None,
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: Some(HashMap::from([(
                "url-list".to_owned(),
                BencodeElem::String("https://example.org/path".to_owned()),
            )])),
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.magnet_link().unwrap(),
            "magnet:?xt=urn:btih:074f42efaf8267f137f114f722d4e7d1dcbfbda5\
             &dn=sample&ws=https://example.org/path"
                .to_owned()
        );
    }

    #[test]
    fn magnet_link_with_web_seeds() {
        let torrent = Torrent {
            announce: None,
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: Some(HashMap::from([(
                "url-list".to_owned(),
                BencodeElem::List(vec![
                    BencodeElem::String("https://example.org/path1".to_owned()),
                    BencodeElem::String("https://example.org/path2".to_owned()),
                ]),
            )])),
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.magnet_link().unwrap(),
            "magnet:?xt=urn:btih:074f42efaf8267f137f114f722d4e7d1dcbfbda5\
             &dn=sample&ws=https://example.org/path1&ws=https://example.org/path2"
                .to_owned()
        );
    }

    #[test]
    fn magnet_link_escape() {
        let torrent = Torrent {
            announce: Some("https://example.org/path?a=1&b=hello world".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: Some(HashMap::from([(
                "url-list".to_owned(),
                BencodeElem::String("https://example.org/path?a=1&b=hello world".to_owned()),
            )])),
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.magnet_link().unwrap(),
            "magnet:?xt=urn:btih:074f42efaf8267f137f114f722d4e7d1dcbfbda5\
             &dn=sample&tr=https://example.org/path?a=1%26b=hello+world\
             &ws=https://example.org/path?a=1%26b=hello+world"
                .to_owned()
        );
    }

    #[test]
    fn is_private_ok() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: Some(HashMap::from_iter(
                vec![("private".to_owned(), bencode_elem!(1))].into_iter(),
            )),
        };

        assert!(torrent.is_private());
    }

    #[test]
    fn is_private_no_extra_fields() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert!(!torrent.is_private());
    }

    #[test]
    fn is_private_no_key() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: Some(HashMap::from_iter(
                vec![("".to_owned(), bencode_elem!(1))].into_iter(),
            )),
        };

        assert!(!torrent.is_private());
    }

    #[test]
    fn is_private_incorrect_val_type() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: Some(HashMap::from_iter(
                vec![("private".to_owned(), bencode_elem!("1"))].into_iter(),
            )),
        };

        assert!(!torrent.is_private());
    }

    #[test]
    fn is_private_incorrect_val() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: Some(HashMap::from_iter(
                vec![("private".to_owned(), bencode_elem!(2))].into_iter(),
            )),
        };

        assert!(!torrent.is_private());
    }
}

#[cfg(test)]
mod file_display_tests {
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn file_display_ok() {
        let file = File {
            length: 42,
            path: PathBuf::from("dir1/file"),
            extra_fields: None,
        };

        assert_eq!(
            file.to_string(),
            "dir1/file\n\
             -size: 42 bytes\n\
             ========================================\n"
        );
    }

    #[test]
    fn file_display_with_extra_fields() {
        let file = File {
            length: 42,
            path: PathBuf::from("dir1/file"),
            extra_fields: Some(HashMap::from_iter(
                vec![
                    ("comment2".to_owned(), bencode_elem!("no comment")),
                    ("comment1".to_owned(), bencode_elem!("no comment")),
                ]
                .into_iter(),
            )),
        };

        assert_eq!(
            file.to_string(),
            "dir1/file\n\
             -size: 42 bytes\n\
             -comment1: \"no comment\"\n\
             -comment2: \"no comment\"\n\
             ========================================\n"
        );
    }
}

#[cfg(test)]
mod torrent_display_tests {
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn torrent_display_ok() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.to_string(),
            "sample.torrent\n\
             -announce: url\n\
             -size: 4 bytes\n\
             -piece length: 2 bytes\n\
             -pieces: [[0102], [0304]]\n"
        );
    }

    #[test]
    fn torrent_display_with_announce_list() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: Some(vec![
                vec!["url1".to_owned(), "url2".to_owned()],
                vec!["url3".to_owned(), "url4".to_owned()],
            ]),
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.to_string(),
            "sample.torrent\n\
             -announce: url\n\
             -announce-list: [[url1, url2], [url3, url4]]\n\
             -size: 4 bytes\n\
             -piece length: 2 bytes\n\
             -pieces: [[0102], [0304]]\n"
        );
    }

    #[test]
    fn torrent_display_with_extra_fields() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: Some(HashMap::from_iter(
                vec![
                    ("comment2".to_owned(), bencode_elem!("no comment")),
                    ("comment1".to_owned(), bencode_elem!("no comment")),
                ]
                .into_iter(),
            )),
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.to_string(),
            "sample.torrent\n\
             -announce: url\n\
             -size: 4 bytes\n\
             -piece length: 2 bytes\n\
             -comment1: \"no comment\"\n\
             -comment2: \"no comment\"\n\
             -pieces: [[0102], [0304]]\n"
        );
    }

    #[test]
    fn torrent_display_with_extra_info_fields() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: Some(HashMap::from_iter(
                vec![
                    ("comment2".to_owned(), bencode_elem!("no comment")),
                    ("comment1".to_owned(), bencode_elem!("no comment")),
                ]
                .into_iter(),
            )),
        };

        assert_eq!(
            torrent.to_string(),
            "sample.torrent\n\
             -announce: url\n\
             -size: 4 bytes\n\
             -piece length: 2 bytes\n\
             -comment1: \"no comment\"\n\
             -comment2: \"no comment\"\n\
             -pieces: [[0102], [0304]]\n"
        );
    }

    #[test]
    fn torrent_display_with_multiple_files() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 4,
            files: Some(vec![
                File {
                    length: 2,
                    path: PathBuf::from("dir1/dir2/file1"),
                    extra_fields: None,
                },
                File {
                    length: 2,
                    path: PathBuf::from("dir1/dir2/file2"),
                    extra_fields: None,
                },
            ]),
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        assert_eq!(
            torrent.to_string(),
            "sample.torrent\n\
             -announce: url\n\
             -size: 4 bytes\n\
             -piece length: 2 bytes\n\
             -files:\n\
             [1] dir1/dir2/file1\n\
             -size: 2 bytes\n\
             ========================================\n\
             \n\
             [2] dir1/dir2/file2\n\
             -size: 2 bytes\n\
             ========================================\n\
             \n\
             -pieces: [[0102], [0304]]\n"
        );
    }
}
