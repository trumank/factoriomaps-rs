#![feature(int_roundings)]

pub mod render;

use crossbeam::channel::{Receiver, Sender};
use crossbeam_channel::unbounded;
use std::sync::{Arc, RwLock};

use render::{MessageToMain, MessageToWorker, VirtualFile};

lazy_static::lazy_static! {
    static ref OPEN_FILES: Arc<RwLock<std::collections::HashMap<usize, Box<VirtualFile>>>> = Default::default();

    static ref SR_RESULT: (Sender<MessageToMain>, Receiver<MessageToMain>) = unbounded::<MessageToMain>();
    static ref SR_WORK: (Sender<MessageToWorker>, Receiver<MessageToWorker>) = unbounded::<MessageToWorker>();
}

hooky::define_hook! {
    unsafe fn fopen(c_filename: *const libc::c_char, c_mode: *const libc::c_char) -> *mut libc::FILE {
        unsafe {
            let filename = std::ffi::CStr::from_ptr(c_filename).to_str().unwrap();
            let mode = std::ffi::CStr::from_ptr(c_mode).to_str().unwrap();
            let path = std::path::Path::new(filename);
            if (path.file_name() == Some(std::ffi::OsStr::new("info.json")) || path.extension() == Some(std::ffi::OsStr::new("png"))) && mode.contains('w') {
                let file = Box::new(VirtualFile::new(filename));
                let ptr = (&*file as *const VirtualFile) as *mut libc::FILE;
                OPEN_FILES.write().unwrap().insert(ptr as usize, file);
                ptr
            } else {
                real::fopen(c_filename, c_mode)
            }
        }
    }

    unsafe fn fwrite(ptr: *const libc::c_void, size: libc::size_t, nobj: libc::size_t, stream: *mut libc::FILE) -> libc::size_t {
        unsafe {
            if OPEN_FILES.read().unwrap().contains_key(&(stream as usize)) {
                let hfile = &mut *(stream as *mut VirtualFile);
                let data = std::slice::from_raw_parts(ptr as *const u8, size * nobj);
                hfile.data.extend_from_slice(data);
                return nobj;
            }
            real::fwrite(ptr, size, nobj, stream)
        }
    }
    unsafe fn fflush(file: *mut libc::FILE) -> libc::c_int {
        unsafe {
            if OPEN_FILES.read().unwrap().contains_key(&(file as usize)) {
                return 0;
            }
            real::fflush(file)
        }
    }
    unsafe fn fclose(file: *mut libc::FILE) -> libc::c_int {
        unsafe {
            if let Some(hfile) = OPEN_FILES.write().unwrap().remove(&(file as usize)) {
                if hfile.path.file_name() == Some(std::ffi::OsStr::new("info.json")) {
                    main();
                }
                SR_RESULT.0
                    .send(MessageToMain::File(*hfile))
                    .unwrap();
                return 0;
            }
            real::fclose(file)
        }
    }
}

fn main() {
    let output = std::env::var(render::FBRS_OUTPUT).unwrap();

    let (result_rx, work_tx, result_tx) =
        (SR_RESULT.1.clone(), SR_WORK.0.clone(), SR_RESULT.0.clone());
    std::thread::spawn(move || {
        let res = crossbeam::scope(|scope| {
            render::spawn_threads(&output, scope, SR_WORK.1.clone(), SR_RESULT.0.clone());
            render::main_loop(output, result_rx, work_tx, result_tx);
            unsafe {
                libc::kill(std::process::id() as i32, libc::SIGTERM);
            }
        });
        res.unwrap();
    });
}
