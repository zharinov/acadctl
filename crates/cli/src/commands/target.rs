use std::fmt;
use std::str::FromStr;

use acadctl_rpc::{DrawingId, InstanceId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub instance_id: InstanceId,
    pub drawing_id: DrawingId,
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", self.instance_id, self.drawing_id)
    }
}

#[derive(Debug)]
pub struct ParseTargetError;

impl FromStr for Target {
    type Err = ParseTargetError;

    fn from_str(target: &str) -> Result<Self, Self::Err> {
        let instance_width = target
            .len()
            .checked_sub(DrawingId::HEX_WIDTH)
            .filter(|width| (InstanceId::MIN_HEX_WIDTH..=InstanceId::MAX_HEX_WIDTH).contains(width))
            .ok_or(ParseTargetError)?;

        if !target.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ParseTargetError);
        }

        let (instance_id, drawing_id) = target.split_at(instance_width);
        let instance_id = instance_id.parse().map_err(|_| ParseTargetError)?;
        let drawing_id = drawing_id.parse().map_err(|_| ParseTargetError)?;

        Ok(Self {
            instance_id,
            drawing_id,
        })
    }
}

impl fmt::Display for ParseTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected 8–12 hexadecimal digits from `acadctl list`")
    }
}

impl std::error::Error for ParseTargetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_targets_without_both_ids() {
        assert!("32f3".parse::<Target>().is_err());
        assert!("0fa532f".parse::<Target>().is_err());
        assert!("00032f3".parse::<Target>().is_err());
        assert!("0fa5:32f3".parse::<Target>().is_err());
    }

    #[test]
    fn parses_and_normalizes_contiguous_targets() {
        let target = "00fa532f3".parse::<Target>().unwrap();
        assert_eq!(target.instance_id, InstanceId::new(0xFA5).unwrap());
        assert_eq!(target.drawing_id, DrawingId::new(0x32F3).unwrap());
        assert_eq!(target.to_string(), "0FA532F3");
    }

    #[test]
    fn preserves_the_fixed_width_leading_zeroes() {
        let target = Target {
            instance_id: InstanceId::new(1).unwrap(),
            drawing_id: DrawingId::new(1).unwrap(),
        };

        assert_eq!(target.to_string(), "00010001");
        assert_eq!(target.to_string().parse::<Target>().unwrap(), target);
    }
}
