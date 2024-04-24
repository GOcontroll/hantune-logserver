#![feature(thread_sleep_until)]

use chrono::{DateTime, Utc};
use process_vm_io::ProcessVirtualMemoryIO;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_with::{serde_as, DurationSeconds};
use std::{
    io::{BufWriter, Read, Seek, Write},
    net::TcpListener,
    sync::atomic::AtomicBool,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};



#[serde_as]
#[derive(Serialize)]
struct LogStatus {
    logging: bool,
    #[serde_as(as = "DurationSeconds<u64>")]
    time_passed: Duration,
    #[serde_as(as = "DurationSeconds<u64>")]
    time_left: Duration,
}

#[derive(Serialize)]
struct Error {
    err: String,
}

#[derive(Deserialize, Serialize)]
struct LogRequest {
    name: Option<String>,
    sampletime: u16,
    duration: u64,
    signals: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Request {
    LogRequest(LogRequest),
    StatusRequest,
    CancelLog,
}

#[derive(Clone)]
struct Signal {
    name: String,
    datatype: u8,
    address: usize,
    length: usize,
}

struct UsableSignal {
    io: ProcessVirtualMemoryIO,
    datatype: u8,
    length: usize,
    address: usize,
    buffer: Vec<u8>,
}

fn main() {
    // let test = Request::LogRequest(LogRequest { name: Some("testnaam".to_string()), sampletime: 10, duration: 10, signals: vec!["test1".to_string(), "test2".to_string()] });
    // println!("{}", serde_json::to_string(&test).unwrap());
    let port = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("No port was given as an argument, using port 50000");
        "50000".to_string()
    });
    let socket = TcpListener::bind(format!("0.0.0.0:{}", port)).expect("could not bind to socket");

    let thread_running: AtomicBool = AtomicBool::new(false);
    let mut thread_handle: Option<JoinHandle<()>> = None;
    let mut start_time: Option<Instant> = None;
    let mut end_time: Option<Instant> = None;

    'accept: loop {
        println!("listening on 0.0.0.0:{}", port);
        let (mut stream, addr) = socket.accept().unwrap();
        println!("Connection accepted from {}!", addr);
        loop {
            _ = stream.flush();
            let mut buf = [0u8; 8096];
            // parse request from tcp stream
            println!("Waiting for a request");
            let request: Request = match &stream.read(&mut buf) {
                Err(_) => {
                    println!("connection lost");
                    continue 'accept;
                }
                Ok(count) => {
                    if let Ok(message) = std::str::from_utf8(&buf) {
                        println!("message: {}\nbytes: {}", message, count);
                    }
                    match serde_json::from_slice(&buf[0..*count]) {
                        Ok(request) => request,
                        Err(err) => {
                            println!("received invalid json");
                            let error = Error {
                                err: err.to_string(),
                            };
                            let error_json = serde_json::to_string(&error).unwrap();
                            println!("error message: {}", error_json);
                            match &stream.write(&error_json.as_bytes()) {
                                Ok(_) => continue,
                                Err(_) => continue 'accept,
                            }
                        }
                    }
                }
            };
            // find which request was made and respond accordingly
            let state = unsafe {
                if let Some(handle) = &thread_handle {
                    !handle.is_finished()
                } else {
                    false
                }
            };
            let request = match request {
                Request::LogRequest(request) => {
                    if state {
                        match &stream.write(
                            serde_json::to_string(&Error {
                                err: "Server is already logging".to_string(),
                            })
                            .unwrap()
                            .as_bytes(),
                        ) {
                            Ok(_) => continue,
                            Err(_) => continue 'accept,
                        }
                    }
                    request
                }
                Request::CancelLog => {
                    state.store(false, std::sync::atomic::Ordering::Relaxed);
                    unsafe {
                        if thread_handle.is_some() {
                            _ = thread_handle.take().unwrap().join();
                        }
                    }
                    continue; // stop the logging
                }
                Request::StatusRequest => {
                    let logstatus = unsafe {
                        if end_time.is_some() {
                            LogStatus {
                                logging: state,
                                time_passed: Instant::now().duration_since(start_time.unwrap()),
                                time_left: end_time.unwrap().duration_since(Instant::now()),
                            }
                        } else {
                            LogStatus {
                                logging: state,
                                time_passed: Duration::from_secs(0),
                                time_left: Duration::from_secs(0),
                            }
                        }
                    };
                    match &stream.write(serde_json::to_string(&logstatus).unwrap().as_bytes()) {
                        Ok(_) => continue,
                        Err(_) => continue 'accept,
                    }
                }
            };
            // get the asap2 signals json
            let Ok(signals_file) = std::fs::read_to_string("/usr/simulink/signals.json") else {
                match &stream.write(
                    serde_json::to_string(&Error {
                        err: "Could not get signals locally".to_string(),
                    })
                    .unwrap()
                    .as_bytes(),
                ) {
                    Ok(_) => continue,
                    Err(_) => continue 'accept,
                }
            };
            // parse the json file
            let Ok(signals_json) = serde_json::from_str::<Value>(signals_file.as_str()) else {
                match &stream.write(
                    serde_json::to_string(&Error {
                        err: "Could not get signals locally".to_string(),
                    })
                    .unwrap()
                    .as_bytes(),
                ) {
                    Ok(_) => continue,
                    Err(_) => continue 'accept,
                }
            };
            // turn json into a vector of signals or errors
            let asap_signals: Vec<Result<Signal, String>> = request
                .signals
                .iter()
                .map(|signal| {
                    let Value::Object(signal_value) = &signals_json[signal] else {
                        return Err(format!("could not find signal {}", signal));
                    };
                    let datatype: u8 =
                        serde_json::from_value(signal_value.get("type").unwrap().clone()).unwrap();
                    let address: usize =
                        serde_json::from_value(signal_value.get("address").unwrap().clone())
                            .unwrap();
                    let length: usize =
                        serde_json::from_value(signal_value.get("size").unwrap().clone()).unwrap();
                    Ok(Signal {
                        name: signal.to_owned(),
                        datatype,
                        address,
                        length,
                    })
                })
                .collect();
            // get the errors from the list
            let errors: Vec<String> = asap_signals
                .iter()
                .cloned()
                .map(|signal| {
                    let Err(err) = signal else { return None };
                    Some(err)
                })
                .flatten()
                .collect();
            // if there are errors write error message back to the stream and start listening for another response
            if !errors.is_empty() {
                let json_error = serde_json::to_string(&Error {
                    err: errors.join("\n"),
                })
                .unwrap();
                println!("missing signals: {}", json_error);
                match &stream.write(json_error.as_bytes()) {
                    Ok(_) => continue,
                    Err(_) => continue 'accept,
                }
            }
            // find the process to attach to
            let name = request.name.unwrap_or("GOcontroll_Linux".to_owned());
            let output = std::process::Command::new("pidof")
                .arg("-s")
                .arg(&name)
                .output();

            // get the output of pidof command
            let Ok(output) = output else {
                match &stream.write(
                    serde_json::to_string(&Error {
                        err: "Could not search for process to attach to".to_owned(),
                    })
                    .unwrap()
                    .as_bytes(),
                ) {
                    Ok(_) => continue,
                    Err(_) => continue 'accept,
                }
            };
            if !output.status.success() {
                match &stream.write(
                    serde_json::to_string(&Error {
                        err: format!("Could not find process \"{}\" to attach to", name),
                    })
                    .unwrap()
                    .as_bytes(),
                ) {
                    Ok(_) => continue,
                    Err(_) => continue 'accept,
                }
            }
            // create the logs directory and the actually usable signals that have the pid embedded in them
            _ = std::fs::create_dir("/usr/simulink/logs");
            let pid = std::str::from_utf8(&output.stdout[0..output.stdout.len() - 1])
                .expect("this should return uft8")
                .parse::<u32>()
                .expect("should be int");
            let mut signals: Vec<UsableSignal> = asap_signals
                .iter()
                .flatten()
                .map(|signal| UsableSignal {
                    io: unsafe {
                        process_vm_io::ProcessVirtualMemoryIO::new(pid, signal.address as u64)
                            .unwrap()
                    },
                    datatype: signal.datatype,
                    length: signal.length,
                    address: signal.address,
                    buffer: match signal.datatype {
                        0 | 1 | 10 => Vec::from_iter([0u8; 1]),
                        2 | 3 => Vec::from_iter([0u8; 2]),
                        4 | 5 | 8 => Vec::from_iter([0u8; 4]),
                        6 | 7 | 9 => Vec::from_iter([0u8; 8]),
                        _ => {
                            state.store(false, std::sync::atomic::Ordering::Relaxed);
                            panic!("incorrect datatype in file");
                        }
                    },
                })
                .collect();
            // create the log file
            let timestamp: DateTime<Utc> = std::time::SystemTime::now().into();
            let filename = format!(
                "/usr/simulink/logs/HANtune_log_{}.csv",
                timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            );
            let logfile = std::fs::File::create(&filename)
                .expect("Couldn't start a log file in /usr/simulink/logs");
            // write the first row of the csv
            let mut writer = BufWriter::new(logfile);
            _ = writer.write(b"time,");
            _ = writer.write(request.signals.join(",").as_bytes());
            _ = writer.write(b"\n");
            _ = writer.flush();
            println!("starting log");
            _ = stream.write(
                serde_json::to_string(&LogStatus {
                    logging: true,
                    time_passed: Duration::from_secs(0),
                    time_left: Duration::from_secs(request.duration),
                })
                .unwrap()
                .as_bytes(),
            );
            let handle = std::thread::spawn(move || {
                state.store(true, std::sync::atomic::Ordering::Relaxed);
                let interval = Duration::from_millis(request.sampletime as u64);
                let log_duration = Duration::from_secs(request.duration);
                let start = Instant::now();
                let mut next_time = start.clone();
                unsafe {
                    start_time = Some(start.clone());
                    end_time = Some(start + log_duration);
                }

                let mut time = 0;
                while next_time.duration_since(start) < log_duration
                    && state.load(std::sync::atomic::Ordering::Relaxed)
                {
                    thread::sleep_until(next_time); // thread timer
                    _ = write!(writer, "{},", time);
                    time += request.sampletime as u32;
                    next_time += interval;
                    for signal in signals.iter_mut() {
                        signal
                            .io
                            .read_exact(&mut signal.buffer.as_mut_slice())
                            .map_err(|_| {
                                state.store(false, std::sync::atomic::Ordering::Relaxed);
                                panic!("Could not read from process_vm")
                            })
                            .unwrap();
                        _ = signal
                            .io
                            .seek(std::io::SeekFrom::Start(signal.address as u64)); // because it is represented as a stream we need to seek back to the proper address :(
                        _ = match signal.datatype {
                            0 => write!(writer, "{}", signal.buffer[0]),
                            1 => write!(
                                writer,
                                "{}",
                                &i8::from_ne_bytes(clone_into_array(&signal.buffer[0..1]))
                            ),
                            2 => write!(
                                writer,
                                "{}",
                                &u16::from_ne_bytes(clone_into_array(&signal.buffer[0..2]))
                            ),
                            3 => write!(
                                writer,
                                "{}",
                                &i16::from_ne_bytes(clone_into_array(&signal.buffer[0..2]))
                            ),
                            4 => write!(
                                writer,
                                "{}",
                                &u32::from_ne_bytes(clone_into_array(&signal.buffer[0..4]))
                            ),
                            5 => write!(
                                writer,
                                "{}",
                                &i32::from_ne_bytes(clone_into_array(&signal.buffer[0..4]))
                            ),
                            6 => write!(
                                writer,
                                "{}",
                                &u64::from_ne_bytes(clone_into_array(&signal.buffer[0..8]))
                            ),
                            7 => write!(
                                writer,
                                "{}",
                                &i64::from_ne_bytes(clone_into_array(&signal.buffer[0..8]))
                            ),
                            8 => write!(
                                writer,
                                "{}",
                                &f32::from_ne_bytes(clone_into_array(&signal.buffer[0..4]))
                            ),
                            9 => write!(
                                writer,
                                "{}",
                                &f64::from_ne_bytes(clone_into_array(&signal.buffer[0..8]))
                            ),
                            10 => write!(writer, "{}", signal.buffer[0]),
                            _ => {
                                state.store(false, std::sync::atomic::Ordering::Relaxed);
                                panic!("Unknown datatype");
                            }
                        };
                        _ = writer.write(b",");
                    }
                    _ = writer.seek(std::io::SeekFrom::End(-1));
                    _ = writer.write(b"\n");
                    _ = writer.flush();
                }
                println!("log finished");
            });
            unsafe { thread_handle = Some(handle) };
        }
    }
}

/// turn a slice into a sized array to perform ::from_bytes() operations on
fn clone_into_array<A, T>(slice: &[T]) -> A
where
    A: Default + AsMut<[T]>,
    T: Clone,
{
    let mut a = A::default();
    <A as AsMut<[T]>>::as_mut(&mut a).clone_from_slice(slice);
    a
}
