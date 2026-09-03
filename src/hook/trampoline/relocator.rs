use super::disasm::is_pc_relative;

pub fn relocate_instruction(instruction: u32, original_pc: usize, out_buffer: &mut Vec<u8>) {
    if !is_pc_relative(instruction) {
        out_buffer.extend_from_slice(&instruction.to_le_bytes());
        return;
    }

    // 1. adr/adrp
    if (instruction & 0x9F000000) == 0x10000000 || (instruction & 0x9F000000) == 0x90000000 {
        let is_adrp = (instruction & 0x80000000) != 0;
        let rd = instruction & 0x1F;
        
        let immlo = (instruction >> 29) & 3;
        let immhi = (instruction >> 5) & 0x7FFFF;
        let mut imm = (immhi << 2) | immlo;
        
        if (imm & 0x100000) != 0 {
            imm |= 0xFFE00000;
        }
        let imm = imm as i32 as i64;
        
        let target = if is_adrp {
            ((original_pc as u64 & !0xFFF) as i64 + (imm << 12)) as u64
        } else {
            (original_pc as i64 + imm) as u64
        };
        
        // ldr xd, #8; b #12; .dword target
        let ldr = 0x58000040 | rd;
        let b = 0x14000003u32;
        out_buffer.extend_from_slice(&ldr.to_le_bytes());
        out_buffer.extend_from_slice(&b.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }

    // 2. b/bl
    if (instruction >> 26) == 0b000101 || (instruction >> 26) == 0b100101 {
        let is_bl = (instruction & 0x80000000) != 0;
        let mut imm = instruction & 0x03FFFFFF;
        if (imm & 0x02000000) != 0 {
            imm |= 0xFC000000;
        }
        let imm = imm as i32 as i64;
        let target = (original_pc as i64 + (imm * 4)) as u64;
        
        if !is_bl {
            // ldr x16, #8; br x16; .dword target
            let ldr_x16 = 0x58000050u32;
            let br_x16 = 0xD61F0200u32;
            out_buffer.extend_from_slice(&ldr_x16.to_le_bytes());
            out_buffer.extend_from_slice(&br_x16.to_le_bytes());
            out_buffer.extend_from_slice(&target.to_le_bytes());
        } else {
            // ldr x30, #12; ldr x16, #16; br x16; .dword ret; .dword target
            let ldr_x30 = 0x5800007Eu32;
            let ldr_x16 = 0x58000090u32;
            let br_x16 = 0xD61F0200u32;
            let ret_addr = (original_pc + 4) as u64;
            
            out_buffer.extend_from_slice(&ldr_x30.to_le_bytes());
            out_buffer.extend_from_slice(&ldr_x16.to_le_bytes());
            out_buffer.extend_from_slice(&br_x16.to_le_bytes());
            out_buffer.extend_from_slice(&ret_addr.to_le_bytes());
            out_buffer.extend_from_slice(&target.to_le_bytes());
        }
        return;
    }

    // 3. b.cond
    if (instruction >> 24) == 0b01010100 {
        let mut imm = (instruction >> 5) & 0x7FFFF;
        if (imm & 0x40000) != 0 { imm |= 0xFFF80000; }
        let imm = imm as i32 as i64;
        let target = (original_pc as i64 + (imm * 4)) as u64;
        
        let cond = instruction & 0xF;
        let inv_cond = cond ^ 1;
        
        // b.inv_cond #20; ldr x16, #8; br x16; .dword target
        let b_inv_cond = 0x54000000 | (5 << 5) | inv_cond;
        let ldr_x16 = 0x58000050u32;
        let br_x16 = 0xD61F0200u32;
        
        out_buffer.extend_from_slice(&b_inv_cond.to_le_bytes());
        out_buffer.extend_from_slice(&ldr_x16.to_le_bytes());
        out_buffer.extend_from_slice(&br_x16.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }
    
    // 4. cbz/cbnz
    if ((instruction >> 25) & 0b00111111) == 0b011010 {
        let mut imm = (instruction >> 5) & 0x7FFFF;
        if (imm & 0x40000) != 0 { imm |= 0xFFF80000; }
        let imm = imm as i32 as i64;
        let target = (original_pc as i64 + (imm * 4)) as u64;
        
        let inv_inst = instruction ^ (1 << 24);
        let inv_inst = (inv_inst & !(0x7FFFF << 5)) | (5 << 5);
        
        let ldr_x16 = 0x58000050u32;
        let br_x16 = 0xD61F0200u32;
        
        out_buffer.extend_from_slice(&inv_inst.to_le_bytes());
        out_buffer.extend_from_slice(&ldr_x16.to_le_bytes());
        out_buffer.extend_from_slice(&br_x16.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }
    
    // 5. tbz/tbnz
    if ((instruction >> 25) & 0b00111111) == 0b011011 {
        let mut imm = (instruction >> 5) & 0x3FFF;
        if (imm & 0x2000) != 0 { imm |= 0xFFFFC000; }
        let imm = imm as i32 as i64;
        let target = (original_pc as i64 + (imm * 4)) as u64;
        
        let inv_inst = instruction ^ (1 << 24);
        let inv_inst = (inv_inst & !(0x3FFF << 5)) | (5 << 5);
        
        let ldr_x16 = 0x58000050u32;
        let br_x16 = 0xD61F0200u32;
        
        out_buffer.extend_from_slice(&inv_inst.to_le_bytes());
        out_buffer.extend_from_slice(&ldr_x16.to_le_bytes());
        out_buffer.extend_from_slice(&br_x16.to_le_bytes());
        out_buffer.extend_from_slice(&target.to_le_bytes());
        return;
    }
    
    // 6. ldr literal
    if (instruction & 0x3B000000) == 0x18000000 {
        let mut imm = (instruction >> 5) & 0x7FFFF;
        if (imm & 0x40000) != 0 { imm |= 0xFFF80000; }
        let imm = imm as i32 as i64;
        let target = (original_pc as i64 + (imm * 4)) as u64;
        
        let ldr_literal = (instruction & 0xFF00001F) | (2 << 5);
        let b = 0x14000003u32;
        let value = unsafe { *(target as *const u64) };
        
        out_buffer.extend_from_slice(&ldr_literal.to_le_bytes());
        out_buffer.extend_from_slice(&b.to_le_bytes());
        out_buffer.extend_from_slice(&value.to_le_bytes());
        return;
    }

    panic!("Unhandled PC-relative instruction at {:#x}: {:#010x}", original_pc, instruction);
}
