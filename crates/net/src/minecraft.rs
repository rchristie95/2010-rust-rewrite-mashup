use bevy::prelude::Resource;

use crate::{WireError, WireReader, WireWriter};

#[derive(Clone, Debug, PartialEq)]
pub enum MinecraftMessage {
    Begin {
        seed: i64,
        origin: [f64; 3],
        revision: u64,
        edits: u32,
    },
    Blocks {
        revision: u64,
        changes: Vec<([i32; 3], u32)>,
    },
    Mobs {
        sequence: u32,
        part: u8,
        total: u8,
        mobs: Vec<MinecraftMobVisual>,
    },
    Stock {
        entries: Vec<(u16, u16)>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinecraftMobVisual {
    pub id: u64,
    pub kind: u8,
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub flags: u8,
}

#[derive(Resource, Default)]
pub struct MinecraftSync {
    pub incoming: Vec<MinecraftMessage>,
    pub incoming_intents: Vec<(sim::ClientId, u8, [i32; 3], u16)>,
    pub accept_intents: bool,
}

pub(crate) fn encode(out: &mut WireWriter, message: &MinecraftMessage) {
    match message {
        MinecraftMessage::Begin {
            seed,
            origin,
            revision,
            edits,
        } => {
            out.put_u8(0);
            out.put_u64(*seed as u64);
            for coordinate in origin {
                out.put_u64(coordinate.to_bits());
            }
            out.put_u64(*revision);
            out.put_u32(*edits);
        }
        MinecraftMessage::Blocks { revision, changes } => {
            out.put_u8(1);
            out.put_u64(*revision);
            out.put_u16(changes.len() as u16);
            for (position, block) in changes {
                for coordinate in position {
                    out.put_i32(*coordinate);
                }
                out.put_u32(*block);
            }
        }
        MinecraftMessage::Mobs {
            sequence,
            part,
            total,
            mobs,
        } => {
            out.put_u8(2);
            out.put_u32(*sequence);
            out.put_u8(*part);
            out.put_u8(*total);
            out.put_u8(mobs.len() as u8);
            for mob in mobs {
                out.put_u64(mob.id);
                out.put_u8(mob.kind);
                for value in mob.position {
                    out.put_f32(value);
                }
                out.put_f32(mob.yaw);
                out.put_f32(mob.pitch);
                out.put_f32(mob.head_yaw);
                out.put_u8(mob.flags);
            }
        }
        MinecraftMessage::Stock { entries } => {
            out.put_u8(3);
            out.put_u8(entries.len() as u8);
            for (state, count) in entries {
                out.put_u16(*state);
                out.put_u16(*count);
            }
        }
    }
}

pub(crate) fn decode(input: &mut WireReader<'_>) -> Result<MinecraftMessage, WireError> {
    Ok(match input.get_u8()? {
        0 => {
            let seed = input.get_u64()? as i64;
            let mut origin = [0.0; 3];
            for coordinate in &mut origin {
                *coordinate = f64::from_bits(input.get_u64()?);
                if !coordinate.is_finite() || coordinate.abs() > 30_000_000.0 {
                    return Err(WireError::Malformed("Minecraft origin"));
                }
            }
            let revision = input.get_u64()?;
            let edits = input.get_u32()?;
            if edits > 1_000_000 {
                return Err(WireError::Malformed("Minecraft edit count"));
            }
            MinecraftMessage::Begin {
                seed,
                origin,
                revision,
                edits,
            }
        }
        1 => {
            let revision = input.get_u64()?;
            let count = input.get_u16()? as usize;
            if count > 32 {
                return Err(WireError::Malformed("Minecraft change count"));
            }
            let mut changes = Vec::with_capacity(count);
            for _ in 0..count {
                let position = [input.get_i32()?, input.get_i32()?, input.get_i32()?];
                let block = input.get_u32()?;
                if i64::from(position[0]).abs() > 30_000_000
                    || i64::from(position[2]).abs() > 30_000_000
                    || !(-2_048..=2_048).contains(&position[1])
                    || block > u16::MAX as u32
                {
                    return Err(WireError::Malformed("Minecraft block change"));
                }
                changes.push((position, block));
            }
            MinecraftMessage::Blocks { revision, changes }
        }
        2 => {
            let sequence = input.get_u32()?;
            let part = input.get_u8()?;
            let total = input.get_u8()?;
            let count = input.get_u8()? as usize;
            if total == 0 || total > 8 || part >= total || count > 16 {
                return Err(WireError::Malformed("Minecraft mobs header"));
            }
            let mut mobs = Vec::with_capacity(count);
            for _ in 0..count {
                let id = input.get_u64()?;
                let kind = input.get_u8()?;
                let position = [input.get_f32()?, input.get_f32()?, input.get_f32()?];
                let yaw = input.get_f32()?;
                let pitch = input.get_f32()?;
                let head_yaw = input.get_f32()?;
                let flags = input.get_u8()?;
                if kind > 14
                    || position
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 30_000_000.0)
                    || !yaw.is_finite()
                    || !pitch.is_finite()
                    || !head_yaw.is_finite()
                {
                    return Err(WireError::Malformed("Minecraft mob"));
                }
                mobs.push(MinecraftMobVisual {
                    id,
                    kind,
                    position,
                    yaw,
                    pitch,
                    head_yaw,
                    flags,
                });
            }
            MinecraftMessage::Mobs {
                sequence,
                part,
                total,
                mobs,
            }
        }
        3 => {
            let count = input.get_u8()? as usize;
            if count > 36 {
                return Err(WireError::Malformed("Minecraft stock count"));
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let state = input.get_u16()?;
                let amount = input.get_u16()?;
                if state == 0 || amount == 0 || entries.iter().any(|(id, _)| *id == state) {
                    return Err(WireError::Malformed("Minecraft stock entry"));
                }
                entries.push((state, amount));
            }
            MinecraftMessage::Stock { entries }
        }
        _ => return Err(WireError::Malformed("Minecraft message tag")),
    })
}
