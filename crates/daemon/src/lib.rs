//! Service composition root and `papersd` (#25): the single library writer behind
//! authenticated local IPC. HTTP stays off (#27).
pub mod desktop_worker;
pub mod ipc;
pub mod jobs;
pub mod lifecycle;
