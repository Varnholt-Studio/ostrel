//! Abstract database interface of the Ostrel programming language.
//!
//! Generated server code talks to a database only through the traits in [`api`]. A driver for a
//! new database implements [`api::Driver`], [`api::Connection`] and [`api::Transaction`] in its
//! own crate, without touching the compiler. Drivers never receive SQL text: they receive typed
//! queries and writes and translate them.
//!
//! Status: F1b contract (ARCHITECTURE 6.1) on the shared ids and values of `ostrel_core`, with
//! the query filter and keyset cursor, the change set of `Set` tags (D49) and `Map` entries,
//! and the schema hash of migration plans. Migration steps are not designed yet.

pub mod api;
