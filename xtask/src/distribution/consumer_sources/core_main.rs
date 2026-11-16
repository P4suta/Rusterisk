mod core_contract;

fn main() -> Result<(), std::io::Error> {
    core_contract::verify()?;
    println!("core candidate consumer passed");
    Ok(())
}
