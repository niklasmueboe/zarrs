//! A HTTP store.

use crate::{
    byte_range::ByteRange,
    storage::{ReadableStorageTraits, StorageError, StoreKeyRange},
};

use super::{ReadableStoreExtension, StoreExtension, StoreKey};

use itertools::Itertools;
use reqwest::{
    header::{HeaderValue, CONTENT_LENGTH, RANGE},
    StatusCode, Url,
};
use std::str::FromStr;
use thiserror::Error;

/// A HTTP store.
#[derive(Debug)]
pub struct HTTPStore {
    base_url: Url,
    batch_range_requests: bool,
}

impl ReadableStoreExtension for HTTPStore {}

impl StoreExtension for HTTPStore {}

impl From<reqwest::Error> for StorageError {
    fn from(err: reqwest::Error) -> Self {
        Self::Other(err.to_string())
    }
}

impl From<url::ParseError> for StorageError {
    fn from(err: url::ParseError) -> Self {
        Self::Other(err.to_string())
    }
}

impl HTTPStore {
    /// Create a new HTTP store at a given `base_url`.
    ///
    /// # Errors
    ///
    /// Returns a [`HTTPStoreCreateError`] if `base_url` is not a valid URL.
    pub fn new(base_url: &str) -> Result<HTTPStore, HTTPStoreCreateError> {
        let base_url = Url::from_str(base_url)
            .map_err(|_| HTTPStoreCreateError::InvalidBaseURL(base_url.into()))?;
        Ok(HTTPStore {
            base_url,
            batch_range_requests: true,
        })
    }

    /// Set whether to batch range requests.
    ///
    /// Defaults to true.
    /// Some servers do not fully support multipart ranges and might return an entire resource given such a request.
    /// It may be preferable to disable batched range requests in this case, so that each range request is a single part range.
    pub fn set_batch_range_requests(&mut self, batch_range_requests: bool) {
        self.batch_range_requests = batch_range_requests;
    }

    /// Maps a [`StoreKey`] to a HTTP [`Url`].
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid.
    pub fn key_to_url(&self, key: &StoreKey) -> Result<Url, url::ParseError> {
        let mut url = self.base_url.as_str().to_string();
        if !key.as_str().is_empty() {
            url += &("/".to_string() + key.as_str().strip_prefix('/').unwrap_or(key.as_str()));
        }
        Url::parse(&url)
    }

    fn get_impl(
        &self,
        key: &StoreKey,
        byte_ranges: &[ByteRange],
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let url = self.key_to_url(key)?;
        let client = reqwest::blocking::Client::new();
        let size = self.size_key(key)?;
        let bytes_strs = byte_ranges
            .iter()
            .map(|byte_range| format!("{}-{}", byte_range.start(size), byte_range.end(size) - 1))
            .join(", ");

        let range = HeaderValue::from_str(&format!("bytes={bytes_strs}")).unwrap();
        let response = client.get(url).header(RANGE, range).send()?;

        match response.status() {
            StatusCode::NOT_FOUND => Err(StorageError::KeyNotFound(key.clone())),
            StatusCode::PARTIAL_CONTENT => {
                // TODO: Gracefully handle a response from the server which does not include all requested by ranges
                let mut bytes = response.bytes()?;
                if bytes.len() as u64
                    == byte_ranges
                        .iter()
                        .map(|byte_range| byte_range.length(size))
                        .sum::<u64>()
                {
                    let mut out = Vec::with_capacity(byte_ranges.len());
                    for byte_range in byte_ranges {
                        let bytes_range =
                            bytes.split_to(usize::try_from(byte_range.length(size)).unwrap());
                        out.push(bytes_range.to_vec());
                    }
                    Ok(out)
                } else {
                    Err(StorageError::from(
                        "http partial content response did not include all requested byte ranges",
                    ))
                }
            }
            StatusCode::OK => {
                // Received all bytes
                let bytes = response.bytes()?;
                let mut out = Vec::with_capacity(byte_ranges.len());
                for byte_range in byte_ranges {
                    let start = usize::try_from(byte_range.start(size)).unwrap();
                    let end = usize::try_from(byte_range.end(size)).unwrap();
                    out.push(bytes[start..end].to_vec());
                }
                Ok(out)
            }
            _ => Err(StorageError::from(format!(
                "the http server responded with status {:?} for the byte range request",
                response.status()
            ))),
        }
    }

    fn get_impl_err(
        &self,
        key: &StoreKey,
        byte_ranges: &[ByteRange],
    ) -> Vec<Result<Vec<u8>, StorageError>> {
        let bytes = self.get_impl(key, byte_ranges);
        match bytes {
            Ok(bytes) => bytes.into_iter().map(Ok).collect(),
            Err(err) => (0..byte_ranges.len())
                .map(|_| match &err {
                    StorageError::KeyNotFound(key) => Err(StorageError::KeyNotFound(key.clone())),
                    _ => Err(StorageError::from(err.to_string())),
                })
                .collect(),
        }
    }
}

impl ReadableStorageTraits for HTTPStore {
    fn get(&self, key: &StoreKey) -> Result<Vec<u8>, StorageError> {
        let url = self.key_to_url(key)?;
        let client = reqwest::blocking::Client::new();
        let response = client.get(url).send()?;
        Ok(response.bytes()?.to_vec())
    }

    fn get_partial_values(
        &self,
        key_ranges: &[StoreKeyRange],
    ) -> Vec<Result<Vec<u8>, StorageError>> {
        let mut out: Vec<Result<Vec<u8>, StorageError>> = Vec::with_capacity(key_ranges.len());

        if self.batch_range_requests {
            let mut last_key = None;
            let mut byte_ranges_group = Vec::new();
            for key_range in key_ranges {
                if last_key.is_none() {
                    last_key = Some(&key_range.key);
                }
                let last_key_val = last_key.unwrap();

                if key_range.key != *last_key_val {
                    // Found a new key, so do a batched get of the byte ranges of the last key
                    out.extend(self.get_impl_err(last_key_val, &byte_ranges_group));

                    last_key = Some(&key_range.key);
                    byte_ranges_group.clear();
                }

                byte_ranges_group.push(key_range.byte_range);
            }

            if !byte_ranges_group.is_empty() {
                // Get the byte ranges of the last key
                let last_key_val = last_key.unwrap();
                out.extend(self.get_impl_err(last_key_val, &byte_ranges_group));
            }
        } else {
            for key_range in key_ranges {
                out.push(
                    self.get_impl_err(&key_range.key, &[key_range.byte_range])
                        .remove(0),
                );
            }
        }

        out
    }

    fn size(&self) -> Result<u64, StorageError> {
        Err(StorageError::Unsupported(
            "size() not supported for HTTP store".into(),
        ))
    }

    fn size_key(&self, key: &StoreKey) -> Result<u64, StorageError> {
        let url = self.key_to_url(key)?;
        let client = reqwest::blocking::Client::new();
        let response = client.head(url).send()?;
        let length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|header_value| header_value.to_str().ok())
            .and_then(|header_str| u64::from_str(header_str).ok())
            .ok_or(StorageError::from("content length response is invalid"))?;
        Ok(length)
    }
}

/// A HTTP store creation error.
#[derive(Debug, Error)]
pub enum HTTPStoreCreateError {
    /// An IO error.
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    /// The url is not valid.
    #[error("base url {0} is not valid")]
    InvalidBaseURL(String),
}

#[cfg(test)]
mod tests {
    use crate::{
        array::{Array, DataType},
        array_subset::ArraySubset,
        node::NodePath,
        storage::meta_key,
    };

    use super::*;

    const HTTP_TEST_PATH_REF: &'static str =
        "https://raw.githubusercontent.com/LDeakin/zarrs/main/tests/data/hierarchy.zarr";
    const ARRAY_PATH_REF: &'static str = "/a/baz";

    #[test]
    fn http_store_size() {
        let store = HTTPStore::new(HTTP_TEST_PATH_REF).unwrap();
        let len = store
            .size_key(&meta_key(&NodePath::new(ARRAY_PATH_REF).unwrap()))
            .unwrap();
        assert_eq!(len, 691);
    }

    #[test]
    fn http_store_get() {
        let store = HTTPStore::new(HTTP_TEST_PATH_REF).unwrap();
        let metadata = store
            .get(&meta_key(&NodePath::new(ARRAY_PATH_REF).unwrap()))
            .unwrap();
        let metadata: crate::array::ArrayMetadataV3 = serde_json::from_slice(&metadata).unwrap();
        assert_eq!(metadata.data_type.name(), "float64");
    }

    #[test]
    fn http_store_array() {
        let store = HTTPStore::new(HTTP_TEST_PATH_REF).unwrap();
        let array = Array::new(store.into(), ARRAY_PATH_REF).unwrap();
        assert_eq!(array.data_type(), &DataType::Float64);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn http_store_array_get() {
        const HTTP_TEST_PATH: &'static str =
            "https://raw.githubusercontent.com/LDeakin/zarrs/main/tests/data/array_write_read.zarr";
        const ARRAY_PATH: &'static str = "/group/array";

        let store = HTTPStore::new(HTTP_TEST_PATH).unwrap();
        let array = Array::new(store.into(), ARRAY_PATH).unwrap();
        assert_eq!(array.data_type(), &DataType::Float32);

        // Read the central 2x2 subset of the array
        let subset_2x2 = ArraySubset::new_with_start_shape(vec![3, 3], vec![2, 2]).unwrap(); // the center 2x2 region
        let data_2x2 = array
            .retrieve_array_subset_elements::<f32>(&subset_2x2)
            .unwrap();
        assert_eq!(data_2x2, &[0.1, 0.2, 0.4, 0.5]);

        // let data = array.retrieve_array_subset_ndarray::<f32>(&ArraySubset::new_with_shape(array.shape().to_vec())).unwrap();
        // println!("{data:?}");
    }

    #[cfg(all(feature = "sharding", feature = "gzip", feature = "crc32c"))]
    #[test]
    fn http_store_sharded_array_get() {
        const HTTP_TEST_PATH_SHARDED: &'static str =
            "https://raw.githubusercontent.com/LDeakin/zarrs/main/tests/data/sharded_array_write_read.zarr";
        const ARRAY_PATH_SHARDED: &'static str = "/group/array";

        let store = HTTPStore::new(HTTP_TEST_PATH_SHARDED).unwrap();
        let array = Array::new(store.into(), ARRAY_PATH_SHARDED).unwrap();
        assert_eq!(array.data_type(), &DataType::UInt16);

        // Read the central 2x2 subset of the array
        let subset_2x2 = ArraySubset::new_with_start_shape(vec![3, 3], vec![2, 2]).unwrap(); // the center 2x2 region
        let data_2x2 = array
            .retrieve_array_subset_elements::<u16>(&subset_2x2)
            .unwrap();
        assert_eq!(data_2x2, &[27, 28, 35, 36]);

        // let data = array.retrieve_array_subset_ndarray::<u16>(&ArraySubset::new_with_shape(array.shape().to_vec())).unwrap();
        // println!("{data:?}");
    }
}