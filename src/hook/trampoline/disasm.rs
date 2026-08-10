pub fn is_pc_relative(instruction: u32) -> bool {
    // Check if the instruction is a PC-relative branch, load, or adr/adrp
    let op = instruction >> 25;
    
    // B or BL (unconditional branch)
    if op == 0b000101 || op == 0b100101 { return true; }
    
    // B.cond (conditional branch)
    if (instruction >> 24) == 0b01010100 { return true; }
    
    // CBZ or CBNZ
    if ((instruction >> 24) & 0b01111111) == 0b00110100 { return true; }
    
    // TBZ or TBNZ
    if ((instruction >> 24) & 0b01111111) == 0b00110110 { return true; }
    
    // ADR or ADRP
    if (instruction & 0x9F000000) == 0x10000000 { return true; }
    
    // LDR (literal)
    if (instruction & 0x3B000000) == 0x18000000 { return true; }
    
    false
}
