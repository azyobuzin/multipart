// Copyright 2016 `multipart` Crate Developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.
//! The server-side abstraction for multipart requests. Enabled with the `server` feature.
//!
//! Use this when you are implementing an HTTP server and want to
//! to accept, parse, and serve HTTP `multipart/form-data` requests (file uploads).
//!
//! See the `Multipart` struct for more info.

extern crate buf_redux;
extern crate httparse;
extern crate safemem;
extern crate twoway;

use std::borrow::Borrow;
use std::collections::HashMap;
use std::fs;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::{io, mem};

use tempdir::TempDir;

use self::boundary::BoundaryReader;

use self::field::PrivReadEntry;

pub use self::field::{MultipartField, MultipartFile, MultipartData, ReadEntry, ReadEntryResult};

use self::save::SaveBuilder;

pub use self::save::{Entries, SaveResult};

macro_rules! try_opt (
    ($expr:expr) => (
        match $expr {
            Some(val) => val,
            None => return None,
        }
    );
    ($expr:expr, $before_ret:expr) => (
        match $expr {
            Some(val) => val,
            None => {
                $before_ret;
                return None;
            }
        }
    )
);

macro_rules! try_read_entry {
    ($self_:expr; $try:expr) => (
        match $try {
            Ok(res) => res,
            Err(err) => return ::server::ReadEntryResult::Error($self_, err),
        }
    )
}

mod boundary;
mod field;

#[cfg(feature = "hyper")]
pub mod hyper;

#[cfg(feature = "iron")]
pub mod iron;

#[cfg(feature = "nickel")]
pub mod nickel;

#[cfg(feature = "tiny_http")]
pub mod tiny_http;

pub mod save;

/// The server-side implementation of `multipart/form-data` requests.
///
/// Implements `Borrow<R>` to allow access to the request body, if desired.
pub struct Multipart<B> {
    reader: BoundaryReader<B>,
}

impl Multipart<()> {
    /// If the given `HttpRequest` is a multipart/form-data POST request,
    /// return the request body wrapped in the multipart reader. Otherwise,
    /// returns the original request.
    pub fn from_request<R: HttpRequest>(req: R) -> Result<Multipart<R::Body>, R> {
        //FIXME: move `map` expr to `Some` arm when nonlexical borrow scopes land.
        let boundary = match req.multipart_boundary().map(String::from) {
            Some(boundary) => boundary,
            None => return Err(req),
        };

        Ok(Multipart::with_body(req.body(), boundary))        
    }   
}

impl<B: Read> Multipart<B> {
    /// Construct a new `Multipart` with the given body reader and boundary.
    pub fn with_body<Bnd: Into<String>>(body: B, boundary: Bnd) -> Self {
        Multipart { 
            reader: BoundaryReader::from_reader(body, boundary.into()),
        }
    }

    /// Read the next entry from this multipart request, returning a struct with the field's name and
    /// data. See `MultipartField` for more info.
    ///
    /// ##Warning: Risk of Data Loss
    /// If the previously returned entry had contents of type `MultipartField::File`,
    /// calling this again will discard any unread contents of that entry.
    pub fn read_entry(&mut self) -> io::Result<Option<MultipartField<&mut Self>>> {
        PrivReadEntry::read_entry(self).into_result()
    }

    /// Read the next entry from this multipart request, returning a struct with the field's name and
    /// data. See `MultipartField` for more info.
    pub fn into_entry(self) -> ReadEntryResult<Self> {
        self.read_entry()
    }

    /// Call `f` for each entry in the multipart request.
    /// 
    /// This is a substitute for Rust not supporting streaming iterators (where the return value
    /// from `next()` borrows the iterator for a bound lifetime).
    ///
    /// Returns `Ok(())` when all fields have been read, or the first error.
    pub fn foreach_entry<F>(&mut self, mut foreach: F) -> io::Result<()> where F: FnMut(MultipartField<&mut Self>) {
        loop {
            match self.read_entry() {
                Ok(Some(field)) => foreach(field),
                Ok(None) => return Ok(()),
                Err(err) => return Err(err),
            }
        }
    }

    pub fn save(&mut self) -> SaveBuilder<Self> {
        SaveBuilder::new(self)
    }

    /// Read the request fully, parsing all fields and saving all files in a new temporary
    /// directory under the OS temporary directory. 
    ///
    /// If there is an error in reading the request, returns the partial result along with the
    /// error. See [`SaveResult`](enum.SaveResult.html) for more information.
    #[deprecated = "use `.save().temp()` instead"]
    pub fn save_all(&mut self) -> SaveResult {
        self.save().temp()
    }

    /// Read the request fully, parsing all fields and saving all files in a new temporary
    /// directory under `dir`. 
    ///
    /// If there is an error in reading the request, returns the partial result along with the
    /// error. See [`SaveResult`](enum.SaveResult.html) for more information.
    #[deprecated = "use `.save().with_temp_dir()` instead"]
    pub fn save_all_under<P: AsRef<Path>>(&mut self, dir: P) -> SaveResult {
        match TempDir::new_in(dir, "multipart") {
            Ok(temp_dir) => self.save().with_temp_dir(temp_dir),
            Err(err) => return SaveResult::Error(err),
        }
    }

    /// Read the request fully, parsing all fields and saving all fields in a new temporary
    /// directory under the OS temporary directory.
    ///
    /// Files larger than `limit` will be truncated to `limit`.
    ///
    /// If there is an error in reading the request, returns the partial result along with the
    /// error. See [`SaveResult`](enum.SaveResult.html) for more information.
    #[deprecated = "use `.save().limit(limit)` instead"]
    pub fn save_all_limited(&mut self, limit: u64) -> SaveResult {
        self.save().limit(limit).temp()
    }

    /// Read the request fully, parsing all fields and saving all files in a new temporary
    /// directory under `dir`. 
    ///
    /// Files larger than `limit` will be truncated to `limit`.
    ///
    /// If there is an error in reading the request, returns the partial result along with the
    /// error. See [`SaveResult`](enum.SaveResult.html) for more information.
    #[deprecated = "use `.save().limit(limit).with_temp_dir()` instead"]
    pub fn save_all_under_limited<P: AsRef<Path>>(&mut self, dir: P, limit: u64) -> SaveResult {
        match TempDir::new_in(dir, "multipart") {
            Ok(temp_dir) => self.save().limit(limit).with_temp_dir(temp_dir),
            Err(err) => return SaveResult::Error(err),
        }
    }
}

impl<R> Borrow<R> for Multipart<R> {
    fn borrow(&self) -> &R {
        self.reader.borrow()
    }
}

impl<R: Read> PrivReadEntry for Multipart<R> {
    type Source = BoundaryReader<R>;

    fn source(&mut self) -> &mut BoundaryReader<R> {
        &mut self.reader
    }

    /// Consume the next boundary.
    /// Returns `true` if the last boundary was read, `false` otherwise.
    fn consume_boundary(&mut self) -> io::Result<bool> {
        debug!("Consume boundary!");
        self.reader.consume_boundary()
    }
}

/// A server-side HTTP request that may or may not be multipart.
///
/// May be implemented by mutable references if providing the request or body by-value is
/// undesirable.
pub trait HttpRequest {
    /// The body of this request.
    type Body: Read;
    /// Get the boundary string of this request if it is a POST request
    /// with the `Content-Type` header set to `multipart/form-data`.
    ///
    /// The boundary string should be supplied as an extra value of the `Content-Type` header, e.g.
    /// `Content-Type: multipart/form-data; boundary={boundary}`.
    fn multipart_boundary(&self) -> Option<&str>;

    /// Return the request body for reading.
    fn body(self) -> Self::Body;
}
