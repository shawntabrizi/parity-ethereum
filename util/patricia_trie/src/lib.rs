// Copyright 2015-2017 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

//! Trie interface and implementation.
extern crate rand;
extern crate ethereum_types;
extern crate keccak_hash as keccak;
extern crate rlp;
extern crate hashdb;
extern crate ethcore_bytes as bytes;
extern crate elastic_array;
extern crate memorydb;
extern crate ethcore_logger;

#[cfg(test)]
extern crate trie_standardmap as standardmap;

#[macro_use]
extern crate log;

use std::{fmt, error};
use hashdb::{HashDB, DBValue, Hasher};
use std::marker::PhantomData;

pub mod node;
pub mod node_codec;
pub mod triedb;
pub mod triedbmut;
pub mod sectriedb;
pub mod sectriedbmut;
pub mod recorder;

mod fatdb;
mod fatdbmut;
mod lookup;
mod nibbleslice;
mod nibblevec;

pub use self::triedb::{TrieDB, TrieDBIterator};
pub use self::triedbmut::TrieDBMut;
pub use self::sectriedbmut::SecTrieDBMut;
pub use self::sectriedb::SecTrieDB;
pub use self::fatdb::{FatDB, FatDBIterator};
pub use self::fatdbmut::FatDBMut;
pub use self::recorder::Recorder;
use node_codec::NodeCodec;

/// Trie Errors.
///
/// These borrow the data within them to avoid excessive copying on every
/// trie operation.
#[derive(Debug)]
pub enum TrieError<T> {
	/// Attempted to create a trie with a state root not in the DB.
	InvalidStateRoot(T),
	/// Trie item not found in the database,
	IncompleteDatabase(T),
	/// Corrupt Trie item
	DecoderError(T, Box<error::Error>),
}

impl<T> fmt::Display for TrieError<T> where T: std::fmt::Debug {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match *self {
			TrieError::InvalidStateRoot(ref root) => write!(f, "Invalid state root: {:?}", root),
			TrieError::IncompleteDatabase(ref missing) => write!(f, "Database missing expected key: {:?}", missing),
			TrieError::DecoderError(ref hash, ref decoder_err) =>  write!(f, "Decoding failed for hash {:?}; err: {:?}", hash, decoder_err),
		}
	}
}

impl<T> error::Error for TrieError<T> where T: std::fmt::Debug {
	fn description(&self) -> &str {
		match *self {
			TrieError::InvalidStateRoot(_) => "Invalid state root",
			TrieError::IncompleteDatabase(_) => "Incomplete database",
			TrieError::DecoderError(_, ref err) => err.description(),
		}
	}
}

/// Trie result type. Boxed to avoid copying around extra space for the `Hasher`s `Out`s on successful queries.
pub type Result<T, U> = ::std::result::Result<T, Box<TrieError<U>>>;

/// Trie-Item type used for iterators over trie data.
pub type TrieItem<'a, U> = Result<(Vec<u8>, DBValue), U>;

/// Description of what kind of query will be made to the trie.
///
/// This is implemented for any &mut recorder (where the query will return
/// a DBValue), any function taking raw bytes (where no recording will be made),
/// or any tuple of (&mut Recorder, FnOnce(&[u8]))
pub trait Query<H: Hasher> {
	/// Output item.
	type Item;

	/// Decode a byte-slice into the desired item.
	fn decode(self, data: &[u8]) -> Self::Item;

	/// Record that a node has been passed through.
	#[allow(unused_variables)]
	fn record(&mut self, hash: &H::Out, data: &[u8], depth: u32) {}
}

impl<'a, H: Hasher> Query<H> for &'a mut Recorder<H> {
	type Item = DBValue;
	fn decode(self, value: &[u8]) -> DBValue { DBValue::from_slice(value) }
	fn record(&mut self, hash: &H::Out, data: &[u8], depth: u32) {
		(&mut **self).record(hash, data, depth);
	}
}

impl<F, T, H: Hasher> Query<H> for F where F: for<'a> FnOnce(&'a [u8]) -> T {
	type Item = T;
	fn decode(self, value: &[u8]) -> T { (self)(value) }
}

impl<'a, F, T, H: Hasher> Query<H> for (&'a mut Recorder<H>, F) where F: FnOnce(&[u8]) -> T {
	type Item = T;
	fn decode(self, value: &[u8]) -> T { (self.1)(value) }
	fn record(&mut self, hash: &H::Out, data: &[u8], depth: u32) {
		self.0.record(hash, data, depth)
	}
}

/// A key-value datastore implemented as a database-backed modified Merkle tree.
pub trait Trie {
	type H: Hasher;
	/// Return the root of the trie.
	fn root(&self) -> &<Self::H as Hasher>::Out;

	/// Is the trie empty?
	fn is_empty(&self) -> bool { *self.root() == Self::H::HASHED_NULL_RLP }

	/// Does the trie contain a given key?
	fn contains(&self, key: &[u8]) -> Result<bool, <Self::H as Hasher>::Out> {
		self.get(key).map(|x|x.is_some() )
	}

	/// What is the value of the given key in this trie?
	fn get<'a, 'key>(&'a self, key: &'key [u8]) -> Result<Option<DBValue>, <Self::H as Hasher>::Out> where 'a: 'key {
		self.get_with(key, DBValue::from_slice)
	}

	/// Search for the key with the given query parameter. See the docs of the `Query`
	/// trait for more details.
	fn get_with<'a, 'key, Q: Query<Self::H>>(&'a self, key: &'key [u8], query: Q) -> Result<Option<Q::Item>, <Self::H as Hasher>::Out> where 'a: 'key;

	/// Returns a depth-first iterator over the elements of trie.
	fn iter<'a>(&'a self) -> Result<Box<TrieIterator<Self::H, Item = TrieItem<Self::H>> + 'a>, <Self::H as Hasher>::Out>;
}

/// A key-value datastore implemented as a database-backed modified Merkle tree.
pub trait TrieMut {
	type H: Hasher;
	/// Return the root of the trie.
	fn root(&mut self) -> &<Self::H as Hasher>::Out;

	/// Is the trie empty?
	fn is_empty(&self) -> bool;

	/// Does the trie contain a given key?
	fn contains(&self, key: &[u8]) -> Result<bool, <Self::H as Hasher>::Out> {
		self.get(key).map(|x| x.is_some())
	}

	/// What is the value of the given key in this trie?
	fn get<'a, 'key>(&'a self, key: &'key [u8]) -> Result<Option<DBValue>, <Self::H as Hasher>::Out> where 'a: 'key;

	/// Insert a `key`/`value` pair into the trie. An empty value is equivalent to removing
	/// `key` from the trie. Returns the old value associated with this key, if it existed.
	fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<Option<DBValue>, <Self::H as Hasher>::Out>;

	/// Remove a `key` from the trie. Equivalent to making it equal to the empty
	/// value. Returns the old value associated with this key, if it existed.
	fn remove(&mut self, key: &[u8]) -> Result<Option<DBValue>, <Self::H as Hasher>::Out>;
}

/// A trie iterator that also supports random access (`seek()`).
pub trait TrieIterator<H: Hasher>: Iterator {
	/// Position the iterator on the first element with key > `key`
	fn seek(&mut self, key: &[u8]) -> Result<(), H::Out>;
}

/// Trie types
#[derive(Debug, PartialEq, Clone)]
pub enum TrieSpec {
	/// Generic trie.
	Generic,
	/// Secure trie.
	Secure,
	///	Secure trie with fat database.
	Fat,
}

impl Default for TrieSpec {
	fn default() -> TrieSpec {
		TrieSpec::Secure
	}
}

/// Trie factory.
#[derive(Default, Clone)]
pub struct TrieFactory<H: Hasher, C: NodeCodec<H>> {
	spec: TrieSpec,
	mark_hash: PhantomData<H>,
	mark_codec: PhantomData<C>,
}

/// All different kinds of tries.
/// This is used to prevent a heap allocation for every created trie.
pub enum TrieKinds<'db, H: Hasher + 'db, C: NodeCodec<H>> {
	/// A generic trie db.
	Generic(TrieDB<'db, H, C>),
	/// A secure trie db.
	Secure(SecTrieDB<'db, H, C>),
	/// A fat trie db.
	Fat(FatDB<'db, H, C>),
}

// wrapper macro for making the match easier to deal with.
macro_rules! wrapper {
	($me: ident, $f_name: ident, $($param: ident),*) => {
		match *$me {
			TrieKinds::Generic(ref t) => t.$f_name($($param),*),
			TrieKinds::Secure(ref t) => t.$f_name($($param),*),
			TrieKinds::Fat(ref t) => t.$f_name($($param),*),
		}
	}
}

impl<'db, H: Hasher, C: NodeCodec<H>> Trie for TrieKinds<'db, H, C> where H::Out: rlp::Decodable + rlp::Encodable {
	type H = H;
	fn root(&self) -> &<Self::H as Hasher>::Out {
		wrapper!(self, root,)
	}

	fn is_empty(&self) -> bool {
		wrapper!(self, is_empty,)
	}

	fn contains(&self, key: &[u8]) -> Result<bool, <Self::H as Hasher>::Out> {
		wrapper!(self, contains, key)
	}

	fn get_with<'a, 'key, Q: Query<Self::H>>(&'a self, key: &'key [u8], query: Q) -> Result<Option<Q::Item>, <Self::H as Hasher>::Out>
		where 'a: 'key
	{
		wrapper!(self, get_with, key, query)
	}

	fn iter<'a>(&'a self) -> Result<Box<TrieIterator<H, Item = TrieItem<H>> + 'a>, <Self::H as Hasher>::Out> {
		wrapper!(self, iter,)
	}
}

impl<'db, H, C> TrieFactory<H, C>
	where H: Hasher, H::Out: rlp::Decodable + rlp::Encodable, C: NodeCodec<H> + 'db
{
	/// Creates new factory.
	pub fn new(spec: TrieSpec) -> Self {
		TrieFactory { spec, mark_hash: PhantomData, mark_codec: PhantomData }
	}

	/// Create new immutable instance of Trie.
	pub fn readonly(&self, db: &'db HashDB<H=H>, root: &'db H::Out) -> Result<TrieKinds<'db, H, C>, H::Out> {
		match self.spec {
			TrieSpec::Generic => Ok(TrieKinds::Generic(TrieDB::new(db, root)?)),
			TrieSpec::Secure => Ok(TrieKinds::Secure(SecTrieDB::new(db, root)?)),
			TrieSpec::Fat => Ok(TrieKinds::Fat(FatDB::new(db, root)?)),
		}
	}

	/// Create new mutable instance of Trie.
	pub fn create(&self, db: &'db mut HashDB<H=H>, root: &'db mut H::Out) -> Box<TrieMut<H=H> + 'db> {
		match self.spec {
			TrieSpec::Generic => Box::new(TrieDBMut::<_, C>::new(db, root)),
			TrieSpec::Secure => Box::new(SecTrieDBMut::<_, C>::new(db, root)),
			TrieSpec::Fat => Box::new(FatDBMut::<_, C>::new(db, root)),
		}
	}

	/// Create new mutable instance of trie and check for errors.
	pub fn from_existing(&self, db: &'db mut HashDB<H=H>, root: &'db mut H::Out) -> Result<Box<TrieMut<H=H> + 'db>, H::Out> {
		match self.spec {
			TrieSpec::Generic => Ok(Box::new(TrieDBMut::<_, C>::from_existing(db, root)?)),
			TrieSpec::Secure => Ok(Box::new(SecTrieDBMut::<_, C>::from_existing(db, root)?)),
			TrieSpec::Fat => Ok(Box::new(FatDBMut::<_, C>::from_existing(db, root)?)),
		}
	}

	/// Returns true iff the trie DB is a fat DB (allows enumeration of keys).
	pub fn is_fat(&self) -> bool { self.spec == TrieSpec::Fat }
}
