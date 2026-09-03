pub fn is_pc_relative(instruction: u32) -> bool {
    let op = instruction >> 25;
    
    // b/bl
    if op == 0b000101 || op == 0b100101 { return true; }
    
    // b.cond
    if (instruction >> 24) == 0b01010100 { return true; }
    
    // cbz/cbnz
    if ((instruction >> 24) & 0b01111111) == 0b00110100 { return true; }
    
    // tbz/tbnz
    if ((instruction >> 24) & 0b01111111) == 0b00110110 { return true; }
    
    // adr/adrp
    if (instruction & 0x9F000000) == 0x10000000 || (instruction & 0x9F000000) == 0x90000000 { return true; }
    
    // ldr literal
    if (instruction & 0x3B000000) == 0x18000000 { return true; }
    
    false
}
