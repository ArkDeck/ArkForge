
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_control_facts_must_reproduce_every_expected_pair() {
        let expected = vec![
            KeyValue {
                key: "const.ohos.fullname".into(),
                value: "OpenHarmony-7.0.0.37".into(),
            },
            KeyValue {
                key: "const.product.model".into(),
                value: "ohos".into(),
            },
        ];
        assert_eq!(
            missing_expected_control_facts(&expected, &expected[..1]),
            ["const.product.model"]
        );
        assert!(missing_expected_control_facts(&expected, &expected).is_empty());

        let wrong_model = vec![
            expected[0].clone(),
            KeyValue {
                key: "const.product.model".into(),
                value: "replacement".into(),
            },
        ];
        assert_eq!(
            missing_expected_control_facts(&expected, &wrong_model),
            ["const.product.model"]
        );
    }

    #[test]
    fn receipt_fact_keys_are_unique_opaque_ids() {
        assert!(
            validate_receipt_fact_keys(&[KeyValue {
                key: "const.product.model".into(),
                value: "ohos".into(),
            }])
            .is_ok()
        );
        assert!(matches!(
            validate_receipt_fact_keys(&[KeyValue {
                key: "has whitespace".into(),
                value: "x".into(),
            }]),
            Err(JobError::InvalidReceiptFactKey(_))
        ));
        assert!(matches!(
            validate_receipt_fact_keys(&[
                KeyValue {
                    key: "mode".into(),
                    value: "one".into(),
                },
                KeyValue {
                    key: "mode".into(),
                    value: "two".into(),
                },
            ]),
            Err(JobError::DuplicateReceiptFactKey(_))
        ));
    }
}
