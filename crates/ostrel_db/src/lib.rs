//! Abstract database interface of the Ostrel programming language.
//!
//! Generated server code talks to a database only through the traits in [`api`]. A driver for a
//! new database implements [`api::Driver`], [`api::Connection`] and [`api::Transaction`] in its
//! own crate, without touching the compiler. Drivers never receive SQL text: they receive typed
//! queries and writes and translate them.
//!
//! Status: first version of the contract (ARCHITECTURE 6.1). The id and value types in [`api`]
//! are local stand ins until the shared types of `ostrel_core` exist; the query filter, keyset
//! cursor and migration steps are not designed yet.

pub mod api;
