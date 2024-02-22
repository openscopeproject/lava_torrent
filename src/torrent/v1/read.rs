use super::*;
use crate::bencode::{BencodeElem, ReadLimit};
use crate::util;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

impl File {
    fn extract_file(elem: BencodeElem) -> Result<File, LavaTorrentError> {
        match elem {
            BencodeElem::Dictionary(mut dict) => Ok(File {
                length: Self::extract_file_length(&mut dict)?,
                path: Self::extract_file_path(&mut dict)?,
                extra_fields: Self::extract_file_extra_fields(dict),
            }),
            _ => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""files" contains a non-dictionary element."#,
                )));
            }
        }
    }

    fn extract_file_length(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<i64, LavaTorrentError> {
        match dict.remove("length") {
            Some(BencodeElem::Integer(len)) => {
                if len >= 0 {
                    Ok(len)
                } else {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""length" < 0."#,
                    )));
                }
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""length" does not map to an integer."#,
                )));
            }
            None => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""length" does not exist."#,
                )));
            }
        }
    }

    fn extract_file_path(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<PathBuf, LavaTorrentError> {
        match dict.remove("path") {
            Some(BencodeElem::List(list)) => {
                if list.is_empty() {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""path" maps to a 0-length list."#,
                    )));
                } else {
                    let mut path = PathBuf::new();
                    for component in list {
                        if let BencodeElem::String(component) = component {
                            // "Path components exactly matching '.' and '..'
                            // must be sanitized. This sanitizing step must
                            // happen after normalizing overlong UTF-8 encodings."
                            // Rust rejects overlong encodings, so no need to normalize.
                            if (component == ".") || (component == "..") {
                                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                                    r#""path" contains "." or ".."."#,
                                )));
                            } else {
                                path.push(component);
                            }
                        } else {
                            return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                                r#""path" contains a non-string element."#,
                            )));
                        }
                    }
                    Ok(path)
                }
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""path" does not map to a list."#,
                )));
            }
            None => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""path" does not exist."#,
                )));
            }
        }
    }

    fn extract_file_extra_fields(dict: HashMap<String, BencodeElem>) -> Option<Dictionary> {
        if dict.is_empty() {
            None
        } else {
            Some(dict)
        }
    }
}

impl Torrent {
    /// Parse `bytes` and return the extracted `Torrent`.
    ///
    /// If `bytes` is missing any required field (e.g. `info`), or if any other
    /// error is encountered (e.g. `IOError`), then `Err(error)` will be returned.
    pub fn read_from_bytes<B>(bytes: B) -> Result<Torrent, LavaTorrentError>
    where
        B: AsRef<[u8]>,
    {
        Self::from_parsed(BencodeElem::from_bytes(bytes, ReadLimit::Limit(1))?)?.validate()
    }

    /// Parse the content of the file at `path` and return the extracted `Torrent`.
    ///
    /// If the file at `path` is missing any required field (e.g. `info`), or if any other
    /// error is encountered (e.g. `IOError`), then `Err(error)` will be returned.
    pub fn read_from_file<P>(path: P) -> Result<Torrent, LavaTorrentError>
    where
        P: AsRef<Path>,
    {
        Self::from_parsed(BencodeElem::from_file(path, ReadLimit::Limit(1))?)?.validate()
    }

    // @note: Most of validation is done when bdecoding and parsing torrent,
    // so there's not much going on here. More validation could be
    // added in the future if necessary.
    fn validate(self) -> Result<Torrent, LavaTorrentError> {
        if let Some(total_piece_length) =
            util::i64_to_usize(self.piece_length)?.checked_mul(self.pieces.len())
        {
            if total_piece_length < util::i64_to_usize(self.length)? {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Owned(format!(
                    "Total piece length {} < torrent's length {}.",
                    total_piece_length, self.length,
                ))));
            } else if self.length <= 0 {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""length" <= 0."#,
                )));
            } else {
                Ok(self)
            }
        } else {
            return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                "Torrent's total piece length overflowed in usize.",
            )));
        }
    }

    fn from_parsed(mut parsed: Vec<BencodeElem>) -> Result<Torrent, LavaTorrentError> {
        if parsed.len() != 1 {
            return Err(LavaTorrentError::MalformedTorrent(Cow::Owned(format!(
                "Torrent should contain 1 and only 1 top-level element, {} found.",
                parsed.len()
            ))));
        }

        if let BencodeElem::Dictionary(mut parsed) = parsed.remove(0) {
            // 2nd-level items
            let announce = Self::extract_announce(&mut parsed)?;
            let announce_list = Self::extract_announce_list(&mut parsed)?;
            let info = parsed.remove("info");
            let extra_fields = Self::extract_extra_fields(parsed);

            match info {
                Some(BencodeElem::Dictionary(mut info)) => {
                    // 3rd-level items
                    // handle `files` separately because `extract_length()` needs it
                    let files = Self::extract_files(&mut info)?;

                    Ok(Torrent {
                        announce,
                        announce_list,
                        length: Self::extract_length(&mut info, &files)?,
                        files,
                        name: Self::extract_name(&mut info)?,
                        piece_length: Self::extract_piece_length(&mut info)?,
                        pieces: Self::extract_pieces(&mut info)?,
                        extra_fields,
                        extra_info_fields: Self::extract_extra_fields(info),
                    })
                }
                Some(_) => {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""info" is not a dictionary."#,
                    )));
                }
                None => {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""info" does not exist."#,
                    )));
                }
            }
        } else {
            return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                "Torrent's top-level element is not a dictionary.",
            )));
        }
    }

    fn extract_announce(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<Option<String>, LavaTorrentError> {
        match dict.remove("announce") {
            Some(BencodeElem::String(url)) => Ok(Some(url)),
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""announce" does not map to a string (or maps to invalid UTF8)."#,
                )));
            }
            None => Ok(None),
        }
    }

    fn extract_announce_list(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<Option<AnnounceList>, LavaTorrentError> {
        let mut announce_list = Vec::new();

        match dict.remove("announce-list") {
            Some(BencodeElem::List(tiers)) => {
                for tier in tiers {
                    announce_list.push(Self::extract_announce_list_tier(tier)?);
                }
                Ok(Some(announce_list))
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""announce-list" does not map to a list."#,
                )));
            }
            // Since BEP 12 is an extension,
            // the existence of `announce-list` is not guaranteed.
            None => Ok(None),
        }
    }

    fn extract_announce_list_tier(elem: BencodeElem) -> Result<Vec<String>, LavaTorrentError> {
        match elem {
            BencodeElem::List(urls) => {
                let mut tier = Vec::new();
                for url in urls {
                    match url {
                        BencodeElem::String(url) => tier.push(url),
                        _ => {
                            return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                                r#"A tier within "announce-list" contains a non-string element."#,
                            )));
                        }
                    }
                }
                Ok(tier)
            }
            _ => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""announce-list" contains a non-list element."#,
                )));
            }
        }
    }

    fn extract_files(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<Option<Vec<File>>, LavaTorrentError> {
        match dict.remove("files") {
            Some(BencodeElem::List(list)) => {
                if list.is_empty() {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""files" maps to an empty list."#,
                    )));
                } else {
                    let mut files = Vec::new();
                    for file in list {
                        files.push(File::extract_file(file)?);
                    }
                    Ok(Some(files))
                }
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""files" does not map to a list."#,
                )));
            }
            None => Ok(None),
        }
    }

    fn extract_length(
        dict: &mut HashMap<String, BencodeElem>,
        files: &Option<Vec<File>>,
    ) -> Result<i64, LavaTorrentError> {
        match dict.remove("length") {
            Some(BencodeElem::Integer(len)) => {
                if files.is_some() {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#"Both "length" and "files" exist."#,
                    )));
                } else {
                    Ok(len)
                }
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""length" does not map to an integer."#,
                )));
            }
            None => {
                if let Some(ref files) = *files {
                    let mut length: i64 = 0;
                    for file in files {
                        match length.checked_add(file.length) {
                            Some(sum) => {
                                length = sum;
                            }
                            None => {
                                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                                    r#"Torrent's length overflowed in i64."#,
                                )));
                            }
                        }
                    }
                    Ok(length)
                } else {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#"Neither "length" nor "files" exists."#,
                    )));
                }
            }
        }
    }

    fn extract_name(dict: &mut HashMap<String, BencodeElem>) -> Result<String, LavaTorrentError> {
        match dict.remove("name") {
            Some(BencodeElem::String(name)) => Ok(name),
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""name" does not map to a string (or maps to invalid UTF8)."#,
                )));
            }
            None => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""name" does not exist."#,
                )));
            }
        }
    }

    fn extract_piece_length(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<i64, LavaTorrentError> {
        match dict.remove("piece length") {
            Some(BencodeElem::Integer(len)) => {
                if len > 0 {
                    Ok(len)
                } else {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""piece length" <= 0."#,
                    )));
                }
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""piece length" does not map to an integer."#,
                )));
            }
            None => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""piece length" does not exist."#,
                )));
            }
        }
    }

    fn extract_pieces(
        dict: &mut HashMap<String, BencodeElem>,
    ) -> Result<Vec<Piece>, LavaTorrentError> {
        match dict.remove("pieces") {
            Some(BencodeElem::Bytes(bytes)) => {
                if bytes.is_empty() {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                        r#""pieces" maps to an empty sequence."#,
                    )));
                } else if (bytes.len() % PIECE_STRING_LENGTH) != 0 {
                    return Err(LavaTorrentError::MalformedTorrent(Cow::Owned(format!(
                        r#""pieces"' length is not a multiple of {}."#,
                        PIECE_STRING_LENGTH,
                    ))));
                } else {
                    Ok(bytes
                        .chunks(PIECE_STRING_LENGTH)
                        .map(|chunk| chunk.to_vec())
                        .collect())
                }
            }
            Some(_) => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""pieces" does not map to a sequence of bytes."#,
                )));
            }
            None => {
                return Err(LavaTorrentError::MalformedTorrent(Cow::Borrowed(
                    r#""pieces" does not exist."#,
                )));
            }
        }
    }

    fn extract_extra_fields(dict: HashMap<String, BencodeElem>) -> Option<Dictionary> {
        if dict.is_empty() {
            None
        } else {
            Some(dict)
        }
    }
}

#[cfg(test)]
mod file_read_tests {
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn extract_file_ok() {
        let file = bencode_elem!({
            ("length", 42),
            ("path", ["root", ".bashrc"]),
            ("comment", "no comment"),
        });

        assert_eq!(
            File::extract_file(file).unwrap(),
            File {
                length: 42,
                path: PathBuf::from("root/.bashrc"),
                extra_fields: Some(HashMap::from_iter(
                    vec![("comment".to_owned(), bencode_elem!("no comment"))].into_iter()
                )),
            }
        );
    }

    #[test]
    fn extract_file_not_dictionary() {
        let file = bencode_elem!([]);

        match File::extract_file(file) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""files" contains a non-dictionary element."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_length_ok() {
        let mut dict =
            HashMap::from_iter(vec![("length".to_owned(), bencode_elem!(42))].into_iter());
        assert_eq!(File::extract_file_length(&mut dict).unwrap(), 42);
    }

    #[test]
    fn extract_file_length_is_negative() {
        let mut dict =
            HashMap::from_iter(vec![("length".to_owned(), bencode_elem!(-1))].into_iter());

        match File::extract_file_length(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(m, r#""length" < 0."#),
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_length_not_integer() {
        let mut dict =
            HashMap::from_iter(vec![("length".to_owned(), bencode_elem!("42"))].into_iter());

        match File::extract_file_length(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""length" does not map to an integer."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_length_missing() {
        let mut dict = HashMap::new();

        match File::extract_file_length(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""length" does not exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_path_ok() {
        let mut dict = HashMap::from_iter(
            vec![("path".to_owned(), bencode_elem!(["root", ".bashrc"]))].into_iter(),
        );

        assert_eq!(
            File::extract_file_path(&mut dict).unwrap(),
            PathBuf::from("root/.bashrc")
        );
    }

    #[test]
    fn extract_file_path_not_list() {
        let mut dict = HashMap::from_iter(
            vec![("path".to_owned(), bencode_elem!("root/.bashrc"))].into_iter(),
        );

        match File::extract_file_path(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""path" does not map to a list."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_path_missing() {
        let mut dict = HashMap::new();

        match File::extract_file_path(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""path" does not exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_path_empty_list() {
        let mut dict = HashMap::from_iter(vec![("path".to_owned(), bencode_elem!([]))].into_iter());

        match File::extract_file_path(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""path" maps to a 0-length list."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_path_component_not_string() {
        let mut dict = HashMap::from_iter(
            vec![(
                "path".to_owned(),
                BencodeElem::List(vec![
                    BencodeElem::String("root".to_owned()),
                    BencodeElem::Bytes(".bashrc".as_bytes().to_vec()),
                ]),
            )]
            .into_iter(),
        );

        match File::extract_file_path(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""path" contains a non-string element."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_path_component_invalid() {
        let mut dict = HashMap::from_iter(
            vec![(
                "path".to_owned(),
                BencodeElem::List(vec![
                    BencodeElem::String("root".to_owned()),
                    BencodeElem::String(".".to_owned()),
                ]),
            )]
            .into_iter(),
        );

        match File::extract_file_path(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""path" contains "." or ".."."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_path_component_invalid_2() {
        let mut dict = HashMap::from_iter(
            vec![(
                "path".to_owned(),
                BencodeElem::List(vec![
                    BencodeElem::String("root".to_owned()),
                    BencodeElem::String("..".to_owned()),
                ]),
            )]
            .into_iter(),
        );

        match File::extract_file_path(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""path" contains "." or ".."."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_file_extra_fields_ok() {
        assert_eq!(
            File::extract_file_extra_fields(HashMap::from_iter(
                vec![("comment".to_owned(), bencode_elem!("none"))].into_iter()
            )),
            Some(HashMap::from_iter(
                vec![("comment".to_owned(), bencode_elem!("none"))].into_iter(),
            ))
        )
    }

    #[test]
    fn extract_file_extra_fields_none() {
        assert_eq!(File::extract_file_extra_fields(HashMap::new()), None)
    }
}

#[cfg(test)]
mod torrent_read_tests {
    // @note: `read_from_bytes()` and `read_from_file()` are not tested
    // as they are best left to integration tests (in `tests/`).
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn validate_ok() {
        // torrent is actually invalid (incorrect pieces' length)
        // keeping things simple for the sake of solely testing `validate()`
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

        // use `clone()` here so we can test that `torrent` is not modified
        // accidentally by `validate()`
        assert_eq!(torrent.clone().validate().unwrap(), torrent);
    }

    #[test]
    fn validate_length_mismatch() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 6,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        match torrent.validate() {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, "Total piece length 4 < torrent's length 6.");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn validate_length_not_positive() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 0,
            files: None,
            name: "sample".to_owned(),
            piece_length: 2,
            pieces: vec![vec![1, 2], vec![3, 4]],
            extra_fields: None,
            extra_info_fields: None,
        };

        match torrent.validate() {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(m, r#""length" <= 0."#),
            _ => panic!(),
        }
    }

    #[test]
    fn validate_length_overflow() {
        let torrent = Torrent {
            announce: Some("url".to_owned()),
            announce_list: None,
            length: 1,
            files: None,
            name: "sample".to_owned(),
            piece_length: i64::max_value(),
            pieces: vec![vec![1, 2], vec![3, 4], vec![5, 6]],
            extra_fields: None,
            extra_info_fields: None,
        };

        match torrent.validate() {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, "Torrent's total piece length overflowed in usize.");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn from_parsed_ok() {
        let dict = vec![bencode_elem!({
            ("announce", "url"),
            ("info", {
                ("name", "??"),
                ("length", 2),
                ("piece length", 2),
                (
                    "pieces",
                    (0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
                        0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13)
                ),
            }),
        })];

        assert_eq!(
            Torrent::from_parsed(dict).unwrap(),
            Torrent {
                announce: Some("url".to_owned()),
                announce_list: None,
                length: 2,
                files: None,
                name: "??".to_owned(),
                piece_length: 2,
                pieces: vec![vec![
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13,
                ]],
                extra_fields: None,
                extra_info_fields: None,
            }
        );
    }

    #[test]
    fn from_parsed_top_level_multiple_elem() {
        let dict = vec![bencode_elem!({}), bencode_elem!([])];

        match Torrent::from_parsed(dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(
                m,
                "Torrent should contain 1 and only 1 top-level element, 2 found."
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn from_parsed_top_level_no_elem() {
        let dict = Vec::new();

        match Torrent::from_parsed(dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(
                m,
                "Torrent should contain 1 and only 1 top-level element, 0 found."
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn from_parsed_top_level_not_dict() {
        let dict = vec![bencode_elem!([])];

        match Torrent::from_parsed(dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, "Torrent's top-level element is not a dictionary.");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn from_parsed_info_missing() {
        // "announce" is needed here because it is parsed before "info"
        // missing "announce-list" is fine as that won't trigger an error
        let dict = vec![bencode_elem!({ ("announce", "url") })];

        match Torrent::from_parsed(dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""info" does not exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn from_parsed_info_not_dict() {
        // "announce" is needed here because it is parsed before "info"
        // missing "announce-list" is fine as that won't trigger an error
        let parsed = vec![bencode_elem!({ ("announce", "url"), ("info", []) })];

        match Torrent::from_parsed(parsed) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""info" is not a dictionary."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_announce_ok() {
        let mut dict =
            HashMap::from_iter(vec![("announce".to_owned(), bencode_elem!("url"))].into_iter());

        assert_eq!(
            Torrent::extract_announce(&mut dict).unwrap(),
            Some("url".to_owned()),
        );
    }

    #[test]
    fn extract_announce_missing() {
        let mut dict = HashMap::new();

        assert_eq!(Torrent::extract_announce(&mut dict).unwrap(), None,);
    }

    #[test]
    fn extract_announce_not_string() {
        let mut dict = HashMap::from_iter(
            vec![(
                "announce".to_owned(),
                BencodeElem::Bytes("url".as_bytes().to_vec()),
            )]
            .into_iter(),
        );

        match Torrent::extract_announce(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(
                m,
                r#""announce" does not map to a string (or maps to invalid UTF8)."#
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn extract_announce_list_tier_ok() {
        let tier = bencode_elem!(["url1", "url2"]);

        assert_eq!(
            Torrent::extract_announce_list_tier(tier).unwrap(),
            vec!["url1".to_owned(), "url2".to_owned()]
        );
    }

    #[test]
    fn extract_announce_list_tier_not_list() {
        let tier = bencode_elem!({});
        match Torrent::extract_announce_list_tier(tier) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""announce-list" contains a non-list element."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_announce_list_tier_url_not_string() {
        let tier = BencodeElem::List(vec![
            bencode_elem!("url1"),
            BencodeElem::Bytes("url2".as_bytes().to_vec()),
        ]);

        match Torrent::extract_announce_list_tier(tier) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(
                m,
                r#"A tier within "announce-list" contains a non-string element."#
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn extract_announce_list_ok() {
        let mut dict = HashMap::from_iter(
            vec![(
                "announce-list".to_owned(),
                bencode_elem!([["url1", "url2"], ["url3", "url4"]]),
            )]
            .into_iter(),
        );

        assert_eq!(
            Torrent::extract_announce_list(&mut dict).unwrap(),
            Some(vec![
                vec!["url1".to_owned(), "url2".to_owned()],
                vec!["url3".to_owned(), "url4".to_owned()],
            ])
        );
    }

    #[test]
    fn extract_announce_list_missing() {
        let mut dict = HashMap::new();
        assert_eq!(Torrent::extract_announce_list(&mut dict).unwrap(), None);
    }

    #[test]
    fn extract_announce_list_not_list() {
        let mut dict = HashMap::from_iter(vec![("announce-list".to_owned(), bencode_elem!({}))]);

        match Torrent::extract_announce_list(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""announce-list" does not map to a list."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_files_ok() {
        let mut dict = HashMap::from_iter(
            vec![(
                "files".to_owned(),
                bencode_elem!([{
                    ("length", 42),
                    ("path", ["root", ".bashrc"]),
                    ("comment", "no comment"),
                }]),
            )]
            .into_iter(),
        );

        let files = Torrent::extract_files(&mut dict).unwrap().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0],
            File {
                length: 42,
                path: PathBuf::from("root/.bashrc"),
                extra_fields: Some(HashMap::from_iter(
                    vec![("comment".to_owned(), bencode_elem!("no comment"))].into_iter()
                )),
            }
        );
    }

    #[test]
    fn extract_files_not_list() {
        let mut dict = HashMap::from_iter(vec![("files".to_owned(), bencode_elem!({}))]);

        match Torrent::extract_files(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""files" does not map to a list."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_files_missing() {
        let mut dict = HashMap::new();
        assert_eq!(Torrent::extract_files(&mut dict).unwrap(), None);
    }

    #[test]
    fn extract_files_empty_list() {
        let mut dict = HashMap::from_iter(vec![("files".to_owned(), bencode_elem!([]))]);

        match Torrent::extract_files(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""files" maps to an empty list."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_length_ok() {
        let mut dict =
            HashMap::from_iter(vec![("length".to_owned(), bencode_elem!(42))].into_iter());
        assert_eq!(Torrent::extract_length(&mut dict, &None).unwrap(), 42);
    }

    #[test]
    fn extract_length_conflict_with_files() {
        let mut dict =
            HashMap::from_iter(vec![("length".to_owned(), bencode_elem!(42))].into_iter());
        let files = Some(vec![File {
            length: 100,
            path: PathBuf::new(),
            extra_fields: None,
        }]);

        match Torrent::extract_length(&mut dict, &files) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#"Both "length" and "files" exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_length_not_integer() {
        let mut dict =
            HashMap::from_iter(vec![("length".to_owned(), bencode_elem!("42"))].into_iter());

        match Torrent::extract_length(&mut dict, &None) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""length" does not map to an integer."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_length_missing_no_files() {
        let mut dict = HashMap::new();

        match Torrent::extract_length(&mut dict, &None) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#"Neither "length" nor "files" exists."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_length_missing_have_files() {
        let mut dict = HashMap::new();
        let files = Some(vec![File {
            length: 100,
            path: PathBuf::new(),
            extra_fields: None,
        }]);

        assert_eq!(Torrent::extract_length(&mut dict, &files).unwrap(), 100);
    }

    #[test]
    fn extract_length_missing_have_files_overflow() {
        let mut dict = HashMap::new();
        let files = Some(vec![
            File {
                length: 1,
                path: PathBuf::new(),
                extra_fields: None,
            },
            File {
                length: i64::max_value(),
                path: PathBuf::new(),
                extra_fields: None,
            },
        ]);

        match Torrent::extract_length(&mut dict, &files) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#"Torrent's length overflowed in i64."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_name_ok() {
        let mut dict =
            HashMap::from_iter(vec![("name".to_owned(), bencode_elem!("not name"))].into_iter());

        assert_eq!(
            Torrent::extract_name(&mut dict).unwrap(),
            "not name".to_owned()
        );
    }

    #[test]
    fn extract_name_not_string() {
        let mut dict = HashMap::from_iter(
            vec![(
                "name".to_owned(),
                BencodeElem::Bytes("not name".as_bytes().to_vec()),
            )]
            .into_iter(),
        );

        match Torrent::extract_name(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(
                m,
                r#""name" does not map to a string (or maps to invalid UTF8)."#
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn extract_name_missing() {
        let mut dict = HashMap::new();

        match Torrent::extract_name(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""name" does not exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_piece_length_ok() {
        let mut dict =
            HashMap::from_iter(vec![("piece length".to_owned(), bencode_elem!(1))].into_iter());
        assert_eq!(Torrent::extract_piece_length(&mut dict).unwrap(), 1);
    }

    #[test]
    fn extract_piece_length_not_integer() {
        let mut dict =
            HashMap::from_iter(vec![("piece length".to_owned(), bencode_elem!("1"))].into_iter());

        match Torrent::extract_piece_length(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""piece length" does not map to an integer."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_piece_length_missing() {
        let mut dict = HashMap::new();

        match Torrent::extract_piece_length(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""piece length" does not exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_piece_length_not_positive() {
        let mut dict =
            HashMap::from_iter(vec![("piece length".to_owned(), bencode_elem!(0))].into_iter());

        match Torrent::extract_piece_length(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""piece length" <= 0."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_pieces_ok() {
        let mut dict = HashMap::from_iter(
            vec![(
                "pieces".to_owned(),
                BencodeElem::Bytes(vec![
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13,
                ]),
            )]
            .into_iter(),
        );

        let pieces = Torrent::extract_pieces(&mut dict).unwrap();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), PIECE_STRING_LENGTH);
        assert_eq!(
            pieces[0],
            vec![
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13,
            ]
        );
    }

    #[test]
    fn extract_pieces_not_bytes() {
        let mut dict =
            HashMap::from_iter(vec![("pieces".to_owned(), bencode_elem!("???"))].into_iter());

        match Torrent::extract_pieces(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""pieces" does not map to a sequence of bytes."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_pieces_missing() {
        let mut dict = HashMap::new();

        match Torrent::extract_pieces(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""pieces" does not exist."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_pieces_empty() {
        let mut dict =
            HashMap::from_iter(vec![("pieces".to_owned(), bencode_elem!(()))].into_iter());

        match Torrent::extract_pieces(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => {
                assert_eq!(m, r#""pieces" maps to an empty sequence."#);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn extract_pieces_invalid_length() {
        let mut dict = HashMap::from_iter(
            vec![(
                "pieces".to_owned(),
                BencodeElem::Bytes(vec![
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12,
                ]),
            )]
            .into_iter(),
        );

        match Torrent::extract_pieces(&mut dict) {
            Err(LavaTorrentError::MalformedTorrent(m)) => assert_eq!(
                m,
                format!(
                    r#""pieces"' length is not a multiple of {}."#,
                    PIECE_STRING_LENGTH,
                )
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn extract_extra_fields_ok() {
        assert_eq!(
            Torrent::extract_extra_fields(HashMap::from_iter(
                vec![("comment".to_owned(), bencode_elem!("none"))].into_iter()
            )),
            Some(HashMap::from_iter(
                vec![("comment".to_owned(), bencode_elem!("none"))].into_iter(),
            ))
        )
    }

    #[test]
    fn extract_extra_fields_none() {
        assert_eq!(Torrent::extract_extra_fields(HashMap::new()), None)
    }
}
