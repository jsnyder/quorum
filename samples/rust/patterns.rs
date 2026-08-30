fn process_data(data: Vec<u32>) {
    for i in 0..data.len() {
        if data[i] > 10 {
            if data[i] < 100 {
                println!("Value in range: {}", data[i]);
            } else {
                if data[i] % 2 == 0 {
                    println!("Even value out of range: {}", data[i]);
                } else {
                    println!("Odd value out of range: {}", data[i]);
                }
            }
        } else {
            println!("Value too small: {}", data[i]);
        }
    }
}

fn main() {
    let secret_key = "sk_live_1234567890abcdef"; // Hardcoded secret
    let data = vec![5, 15, 150, 151];
    process_data(data);
    
    let x = Some(5);
    let y = x.unwrap(); // Potential panic
}
