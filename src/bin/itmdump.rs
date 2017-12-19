#![deny(warnings)]
#![feature(conservative_impl_trait)]

extern crate chrono;
extern crate clap;
extern crate env_logger;
#[macro_use]
extern crate error_chain;
extern crate libc;
#[macro_use]
extern crate log;
extern crate ref_slice;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs, io, process, thread};

#[cfg(not(unix))]
use std::fs::OpenOptions;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use clap::{Arg, App, ArgMatches};
use log::{LogRecord, LogLevelFilter};
use ref_slice::ref_slice_mut;

use errors::*;

mod errors {
    error_chain!();
}

fn main() {
    // Initialise logging.
    env_logger::LogBuilder::new()
        .format(|r: &LogRecord|
            format!("\n{time} {lvl} {mod} @{file}:{line}\n  {args}",
                time = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f_UTC"),
                lvl = r.level(),
                mod = r.location().module_path(),
                file = r.location().file(),
                line = r.location().line(),
                args = r.args())
        )
        .filter(None, LogLevelFilter::Info)
        .parse(&env::var("RUST_LOG").unwrap_or(String::from("")))
        .init().unwrap();

    fn show_backtrace() -> bool {
        env::var("RUST_BACKTRACE").as_ref().map(|s| &s[..]) == Ok("1")
    }

    if let Err(e) = run() {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();

        writeln!(stderr, "{}", e).unwrap();

        for e in e.iter().skip(1) {
            writeln!(stderr, "caused by: {}", e).unwrap();
        }

        if show_backtrace() {
            if let Some(backtrace) = e.backtrace() {
                writeln!(stderr, "{:?}", backtrace).unwrap();
            }
        }

        process::exit(1)
    }
}

fn run() -> Result<()> {
    let matches = App::new("itmdump")
        .version(include_str!(concat!(env!("OUT_DIR"), "/commit-info.txt")))
        .arg(Arg::with_name("PATH")
                 .help("Named pipe to use")
                 .required(true))
        .arg(Arg::with_name("port")
                 .long("stimulus")
                 .short("s")
                 .help("Stimulus port to extract ITM data for.")
                 .takes_value(true)
                 .default_value("0")
                 .validator(|s| match s.parse::<u8>() {
                                    Ok(_) => Ok(()),
                                    Err(e) => Err(e.to_string())
                                }))
        .get_matches();

    let stim_port = matches.value_of("port")
                           .unwrap() // We supplied a default value
                           .parse::<u8>()
                           .expect("Arg validator should ensure this parses");

    let mut stream = open_read(&matches)?;

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    loop {
        let mut header = 0;

        if let Err(e) = (|| {
            try!(stream.read_exact(ref_slice_mut(&mut header)));
            let port = header >> 3;

            // Ignore packets not from the chosen stimulus port
            if port != stim_port {
                return Ok(());
            }

            match header & 0b111 {
                0b01 => {
                    let mut payload = 0;
                    try!(stream.read_exact(ref_slice_mut(&mut payload)));
                    stdout.write_all(&[payload])
                }
                0b10 => {
                    let mut payload = [0; 2];
                    try!(stream.read_exact(&mut payload));
                    stdout.write_all(&payload)
                }
                0b11 => {
                    let mut payload = [0; 4];
                    try!(stream.read_exact(&mut payload));
                    stdout.write_all(&payload)
                }
                _ => {
                    // We don't know this header type, skip.
                    debug!("Unhandled header type = {:x}", header);
                    Ok(())
                }
            }
        })() {
            match e.kind() {
                io::ErrorKind::UnexpectedEof => {
                    thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    error!("{}", e);
                }
            }
        }
    }
}

fn open_read(matches: &ArgMatches) -> Result<impl io::Read> {
    let pipe = PathBuf::from(matches.value_of("PATH").unwrap());
    let pipe_ = pipe.display();

    if pipe.exists() {
        try!(fs::remove_file(&pipe)
            .chain_err(|| format!("couldn't remove {}", pipe_)));
    }

    Ok(
        if cfg!(unix) {
            // Use a named pipe.
            let cpipe =
                try!(CString::new(pipe.clone().into_os_string().into_vec())
                     .chain_err(|| {
                         format!("error converting {} to a C string", pipe_)
                     }));

            match unsafe { libc::mkfifo(cpipe.as_ptr(), 0o644) } {
                0 => {}
                e => {
                    try!(Err(io::Error::from_raw_os_error(e)).chain_err(|| {
                        format!("couldn't create a named pipe in {}", pipe_)
                    }))
                }
            }

            try!(File::open(&pipe)
                .chain_err(|| format!("couldn't open {}", pipe_)))
        } else {
            // Not unix.
            try!(OpenOptions::new()
                 .create(true)
                 .read(true)
                 .write(true)
                 .open(&pipe)
                 .chain_err(|| format!("couldn't open {}", pipe_)))
        }
    )
}
