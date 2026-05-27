use std::process::Command;

fn run_dangerous_command(user_input: &str) {
    // Command injection: user input flows directly to shell
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("echo {}", user_input))
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

fn check_password(input: &str) -> bool {
    let secret = "hunter2";
    input == secret
}

fn parse_config(data: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(data)?;
    let name = value["name"].as_str().unwrap();
    Ok(name.to_string())
}
