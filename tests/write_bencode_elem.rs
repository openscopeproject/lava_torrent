extern crate lava_torrent;
extern crate rand;

use lava_torrent::bencode::{BencodeElem, ReadLimit};
use rand::Rng;
use std::collections::HashMap;
use std::iter::FromIterator;

const OUTPUT_ROOT: &str = "tests/tmp/";

fn rand_file_name() -> String {
    OUTPUT_ROOT.to_owned() + &rand::thread_rng().gen::<u16>().to_string()
}

#[test]
fn bencode_elem_write_string_to_file_ok() {
    let original = BencodeElem::String("spam".to_owned());
    let output = rand_file_name();

    original.write_into_file(&output).unwrap();
    let duplicate = BencodeElem::from_file(&output, ReadLimit::NoLimit).unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(original, duplicate[0]);
}

#[test]
fn bencode_elem_write_bytes_to_file_ok() {
    let original = BencodeElem::Bytes(vec![0xff, 0xfe, 0xfd, 0xfc]);
    let output = rand_file_name();

    original.write_into_file(&output).unwrap();
    let duplicate = BencodeElem::from_file(&output, ReadLimit::NoLimit).unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(original, duplicate[0]);
}

#[test]
fn bencode_elem_write_integer_to_file_ok() {
    let original = BencodeElem::Integer(42);
    let output = rand_file_name();

    original.write_into_file(&output).unwrap();
    let duplicate = BencodeElem::from_file(&output, ReadLimit::NoLimit).unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(original, duplicate[0]);
}

#[test]
fn bencode_elem_write_list_to_file_ok() {
    let original = BencodeElem::List(vec![
        BencodeElem::Integer(42),
        BencodeElem::String("spam".to_owned()),
    ]);
    let output = rand_file_name();

    original.write_into_file(&output).unwrap();
    let duplicate = BencodeElem::from_file(&output, ReadLimit::NoLimit).unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(original, duplicate[0]);
}

#[test]
fn bencode_elem_write_dictionary_to_file_ok() {
    let original = BencodeElem::Dictionary(HashMap::from_iter(
        vec![
            ("spam".to_owned(), BencodeElem::Integer(42)),
            ("cow".to_owned(), BencodeElem::String("moo".to_owned())),
        ]
        .into_iter(),
    ));
    let output = rand_file_name();

    original.write_into_file(&output).unwrap();
    let duplicate = BencodeElem::from_file(&output, ReadLimit::NoLimit).unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(original, duplicate[0]);
}
