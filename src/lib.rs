//! hejiu45-oss — a single-binary, S3-compatible object storage server.
//!
//! Crate layout:
//! * [`config`]  — CLI/env surface
//! * [`error`]   — S3 XML errors and console JSON errors
//! * [`crypto`]  — OS randomness, HKDF/AES-GCM envelope encryption, Argon2id
//! * [`meta`]    — redb metadata engine (buckets, objects, keys, sessions)
//! * [`data`]    — filesystem body storage with atomic replacement
//! * [`auth`]    — SigV4 verification, ACLs, presigning
//! * [`s3`]      — S3 protocol handlers
//! * [`admin`]   — console auth + management API
//! * [`web`]     — embedded SPA serving
//! * [`server`]  — router assembly, TLS, graceful shutdown

#![forbid(unsafe_code)]

pub mod admin;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod data;
pub mod error;
pub mod meta;
pub mod s3;
pub mod server;
pub mod state;
pub mod web;

pub use config::PRODUCT;
