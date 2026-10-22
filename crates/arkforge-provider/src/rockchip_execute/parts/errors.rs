
impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionError::RequiresAuthority { control_action } => write!(
                f,
                "{control_action} is performed by the authority's control port, not by this \
                 Provider (architecture.md 9.2)"
            ),
            ExecutionError::ActionUndecodable(detail) => {
                write!(f, "stored action cannot be decoded: {detail}")
            }
            ExecutionError::PortRefused { operation, message } => {
                write!(f, "{operation} was refused before I/O: {message}")
            }
            ExecutionError::ExternalIo { operation, message } => {
                write!(
                    f,
                    "{operation} failed after native USB I/O began: {message}"
                )
            }
            ExecutionError::LayoutMismatch { expected, observed } => write!(
                f,
                "the device's partition table hashes to {observed}, the plan assumed {expected}"
            ),
            ExecutionError::PartitionTableUnreadable(head) => {
                write!(f, "the partition table listing could not be read: {head:?}")
            }
            ExecutionError::DeviceDeclaresUnknownPartitions(names) => write!(
                f,
                "the device declares partitions the profile does not know: {}",
                names.join(", ")
            ),
            ExecutionError::TableNotObservedYet => f.write_str(
                "the device's own partition table has not been observed; no write may be \
                 dispatched against an unread table",
            ),
            ExecutionError::ReadDomainNotCharacterized => f.write_str(
                "the medium's read face has not been characterized; a readback verdict without \
                 it cannot tell filler from an unwritten partition (architecture.md 16.4)",
            ),
            ExecutionError::NoTableAtLba1 => f.write_str(
                "LBA 1 carries no table; the name-addressed write has nothing to resolve against",
            ),
            ExecutionError::TargetNotAllowed(partition) => {
                write!(f, "the profile does not allow writing {partition}")
            }
            ExecutionError::PartitionNotOnDevice(partition) => write!(
                f,
                "the device's own table declares no partition named {partition}"
            ),
            ExecutionError::TargetOffsetDisagrees {
                partition,
                profile,
                plan,
            } => write!(
                f,
                "{partition} starts at sector {profile} in one source and {plan} in another; \
                 refusing the write"
            ),
            ExecutionError::ImageNotStaged(member) => {
                write!(f, "{member} was not staged for this job")
            }
            ExecutionError::ImageOverrunsPartition {
                partition,
                image_sectors,
                partition_sectors,
            } => write!(
                f,
                "the image for {partition} needs {image_sectors} sectors and the device gives it \
                 {partition_sectors}; refusing the write"
            ),
            ExecutionError::StagingChanged(detail) => {
                write!(f, "a staged image changed before it was written: {detail}")
            }
            ExecutionError::VerificationRangeMissing => {
                f.write_str("a verification action carries no declared range or content digest")
            }
            ExecutionError::ReadFailed {
                begin_sector,
                sectors,
                output,
            } => write!(
                f,
                "reading {sectors} sector(s) at {begin_sector} failed: {output}"
            ),
            ExecutionError::ScratchUnusable(detail) => write!(f, "scratch space: {detail}"),
        }
    }
}

impl std::error::Error for ExecutionError {}
