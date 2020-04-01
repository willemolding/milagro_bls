extern crate amcl;
extern crate rand;

use super::amcl_utils::{
    self, ate2_evaluation, hash_to_curve_g2, subgroup_check_g2, Big, GroupG1, GroupG2,
};
use super::errors::DecodeError;
use super::g1::G1Point;
use super::g2::G2Point;
use super::keys::PublicKey;
use super::signature::Signature;
use amcl::bls381::pair;
use rand::Rng;

/// Allows for the adding/combining of multiple BLS PublicKeys.
///
/// This may be used to verify some AggregateSignature.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct AggregatePublicKey {
    pub point: G1Point,
}

impl AggregatePublicKey {
    /// Instantiate a new aggregate public key.
    ///
    /// The underlying point will be set to infinity.
    pub fn new() -> Self {
        Self {
            point: G1Point::new(),
        }
    }

    /// Instantiate a new aggregate public key from a vector of PublicKeys.
    ///
    /// This is a helper method combining the `new()` and `add()` functions.
    pub fn from_public_keys(keys: &[&PublicKey]) -> Self {
        let mut agg_key = AggregatePublicKey::new();
        for key in keys {
            agg_key.point.add(&key.point)
        }
        agg_key.point.affine();
        agg_key
    }

    /// Add a PublicKey to the AggregatePublicKey.
    pub fn add(&mut self, public_key: &PublicKey) {
        self.point.add(&public_key.point);
        //self.point.affine();
    }

    /// Add a AggregatePublicKey to the AggregatePublicKey.
    pub fn add_aggregate(&mut self, aggregate_public_key: &AggregatePublicKey) {
        self.point.add(&aggregate_public_key.point);
        //self.point.affine();
    }

    /// Instantiate an AggregatePublicKey from compressed bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<AggregatePublicKey, DecodeError> {
        let point = G1Point::from_bytes(bytes)?;
        Ok(Self { point })
    }

    /// Export the AggregatePublicKey to compressed bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        self.point.as_bytes()
    }
}

impl Default for AggregatePublicKey {
    fn default() -> Self {
        Self::new()
    }
}

/// Allows for the adding/combining of multiple BLS Signatures.
///
/// This may be verified against some AggregatePublicKey.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct AggregateSignature {
    pub point: G2Point,
}

impl AggregateSignature {
    /// Instantiates a new AggregateSignature.
    ///
    /// The underlying point will be set to infinity.
    pub fn new() -> Self {
        Self {
            point: G2Point::new(),
        }
    }

    /// Add a Signature to the AggregateSignature.
    pub fn add(&mut self, signature: &Signature) {
        self.point.add(&signature.point);
    }

    /// Add a AggregateSignature to the AggregateSignature.
    ///
    /// To maintain consensus AggregateSignatures should only be added
    /// if they relate to the same message
    pub fn add_aggregate(&mut self, aggregate_signature: &AggregateSignature) {
        self.point.add(&aggregate_signature.point);
    }

    /// FastAggregateVerify
    ///
    /// Verifies an AggregateSignature against a list of PublicKeys
    /// https://tools.ietf.org/html/draft-irtf-cfrg-bls-signature-02#section-3.3.4
    pub fn fast_aggregate_verify(&self, msg: &[u8], public_keys: &[&PublicKey]) -> bool {
        // Subgroup check for signature
        if !subgroup_check_g2(self.point.as_raw()) {
            return false;
        }

        // Aggregate PublicKeys
        let aggregate_public_key = AggregatePublicKey::from_public_keys(public_keys);

        // Points must be affine for pairing
        let mut sig_point = self.point.as_raw().clone();
        let mut key_point = aggregate_public_key.point.as_raw().clone();
        let mut msg_hash_point = hash_to_curve_g2(msg);
        sig_point.affine();
        key_point.affine();
        msg_hash_point.affine();

        let mut generator_g1_negative = amcl_utils::GroupG1::generator();
        generator_g1_negative.neg();

        // Faster ate2 evaualtion checks e(S, -G1) * e(H, PK) == 1
        ate2_evaluation(
            &sig_point,
            &generator_g1_negative,
            &msg_hash_point,
            &key_point,
        )
    }

    /// FastAggregateVerify - pre-aggregated PublicKeys
    ///
    /// Verifies an AggregateSignature against an AggregatePublicKey.
    /// Differs to IEFT FastAggregateVerify in that public keys are already aggregated.
    /// https://tools.ietf.org/html/draft-irtf-cfrg-bls-signature-02#section-3.3.4
    pub fn fast_aggregate_verify_pre_aggregated(
        &self,
        msg: &[u8],
        aggregate_public_key: &AggregatePublicKey,
    ) -> bool {
        // Subgroup check for signature
        if !subgroup_check_g2(self.point.as_raw()) {
            return false;
        }

        let mut sig_point = self.point.clone();
        let mut key_point = aggregate_public_key.point.clone();
        sig_point.affine();
        key_point.affine();
        let mut msg_hash_point = hash_to_curve_g2(msg);
        msg_hash_point.affine();

        // Faster ate2 evaualtion checks e(S, -G1) * e(H, PK) == 1
        let mut generator_g1_negative = amcl_utils::GroupG1::generator();
        generator_g1_negative.neg();
        ate2_evaluation(
            &sig_point.as_raw(),
            &generator_g1_negative,
            &msg_hash_point,
            &key_point.as_raw(),
        )
    }

    /// Verify Multiple AggregateSignatures
    ///
    /// Input (AggregateSignature, PublicKey[m], Message(Vec<u8>))[n]
    /// Checks that each AggregateSignature is valid with a reduced number of pairings.
    /// https://ethresear.ch/t/fast-verification-of-multiple-bls-signatures/5407
    pub fn verify_multiple_aggregate_signatures<'a, R, I>(rng: &mut R, signature_sets: I) -> bool
    where
        R: Rng + ?Sized,
        I: Iterator<Item = (&'a AggregateSignature, &'a [&'a PublicKey], &'a [u8])>,
    {
        // Sum of (AggregateSignature[i] * rand[i]) for all AggregateSignatures - S'
        let mut final_agg_sig = GroupG2::new();

        // Stores current value of pairings
        let mut pairing = pair::initmp();

        for (aggregate_signature, public_keys, message) in signature_sets {
            // TODO: Consider increasing rand size from 2^63 to 2^128
            // Create random offset - rand[i]
            let mut rand = 0;
            while rand == 0 {
                // Require: rand > 0
                let mut rand_bytes = [0 as u8; 8]; // bytes
                rng.fill(&mut rand_bytes);
                rand = i64::from_be_bytes(rand_bytes).abs();
            }
            let rand = Big::new_int(rand as isize);

            // Hash message to curve - H(message[i])
            let mut hash_point = hash_to_curve_g2(message);

            // Aggregate PublicKeys - Apk[i]
            let mut aggregate_public_key = AggregatePublicKey::from_public_keys(public_keys)
                .point
                .into_raw();

            // rand[i] * Apk[i]
            aggregate_public_key = aggregate_public_key.mul(&rand);

            // Points must be affine before pairings
            hash_point.affine();
            aggregate_public_key.affine();

            // Update current pairings: *= e(H(message[i]), rand[i] * Apk[i])
            pair::another(&mut pairing, &hash_point, &aggregate_public_key);

            // S' += rand[i] * AggregateSignature[i]
            final_agg_sig.add(&aggregate_signature.point.as_raw().mul(&rand));
        }

        // Pairing for LHS - e(As', G1)
        let mut negative_g1 = GroupG1::generator();
        negative_g1.neg(); // will be affine
        final_agg_sig.affine();
        pair::another(&mut pairing, &final_agg_sig, &negative_g1);

        // Complete pairing and verify output is 1.
        let mut v = pair::miller(&pairing);
        v = pair::fexp(&v);
        v.isunity()
    }

    /// Instatiate an AggregateSignature from some bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<AggregateSignature, DecodeError> {
        let point = G2Point::from_bytes(bytes)?;
        Ok(Self { point })
    }

    /// Export (serialize) the AggregateSignature to bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        self.point.as_bytes()
    }
}

impl Default for AggregateSignature {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate hex;
    extern crate rand;

    use super::super::keys::{Keypair, SecretKey};
    use super::*;

    #[test]
    fn test_aggregate_serialization() {
        let signing_secret_key_bytes = vec![
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 98, 161, 50, 32, 254, 87, 16, 25,
                167, 79, 192, 116, 176, 74, 164, 217, 40, 57, 179, 15, 19, 21, 240, 100, 70, 127,
                111, 170, 129, 137, 42, 53,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 53, 72, 211, 104, 184, 68, 142,
                208, 115, 22, 156, 97, 28, 216, 228, 102, 4, 218, 116, 226, 166, 131, 67, 7, 40,
                55, 157, 167, 157, 127, 143, 13,
            ],
        ];
        let signing_keypairs: Vec<Keypair> = signing_secret_key_bytes
            .iter()
            .map(|bytes| {
                let sk = SecretKey::from_bytes(&bytes).unwrap();
                let pk = PublicKey::from_secret_key(&sk);
                Keypair { sk, pk }
            })
            .collect();

        let message = "cats".as_bytes();

        let mut agg_sig = AggregateSignature::new();
        let mut agg_pub_key = AggregatePublicKey::new();
        for keypair in &signing_keypairs {
            let sig = Signature::new(&message, &keypair.sk);
            agg_sig.add(&sig);
            agg_pub_key.add(&keypair.pk);
        }

        let agg_sig_bytes = agg_sig.as_bytes();
        let agg_pub_bytes = agg_pub_key.as_bytes();

        let agg_sig = AggregateSignature::from_bytes(&agg_sig_bytes).unwrap();
        let agg_pub_key = AggregatePublicKey::from_bytes(&agg_pub_bytes).unwrap();

        assert!(agg_sig.fast_aggregate_verify_pre_aggregated(&message, &agg_pub_key));
    }

    fn map_secret_bytes_to_keypairs(secret_key_bytes: Vec<Vec<u8>>) -> Vec<Keypair> {
        let mut keypairs = vec![];
        for bytes in secret_key_bytes {
            let sk = SecretKey::from_bytes(&bytes).unwrap();
            let pk = PublicKey::from_secret_key(&sk);
            keypairs.push(Keypair { sk, pk })
        }
        keypairs
    }

    // A helper for doing a comprehensive aggregate sig test.
    fn helper_test_aggregate_public_keys(
        control_kp: Keypair,
        signing_kps: Vec<Keypair>,
        non_signing_kps: Vec<Keypair>,
    ) {
        let signing_kps_subset = {
            let mut subset = vec![];
            for i in 0..signing_kps.len() - 1 {
                subset.push(signing_kps[i].clone());
            }
            subset
        };

        let messages = vec![
            "Small msg".as_bytes(),
            "cats lol".as_bytes(),
            &[42_u8; 133700],
        ];

        for message in messages {
            let mut agg_signature = AggregateSignature::new();
            let mut signing_agg_pub = AggregatePublicKey::new();
            for keypair in &signing_kps {
                let sig = Signature::new(&message, &keypair.sk);
                assert!(sig.core_verify(&message, &keypair.pk));
                assert!(!sig.core_verify(&message, &control_kp.pk));
                agg_signature.add(&sig);
                signing_agg_pub.add(&keypair.pk);
            }

            /*
             * The full set of signed keys should pass verification.
             */
            assert!(agg_signature.fast_aggregate_verify_pre_aggregated(&message, &signing_agg_pub));

            /*
             * The full set of signed keys aggregated in reverse order
             * should pass verification.
             */
            let mut rev_signing_agg_pub = AggregatePublicKey::new();
            for i in (0..signing_kps.len()).rev() {
                rev_signing_agg_pub.add(&signing_kps[i].pk);
            }
            assert!(
                agg_signature.fast_aggregate_verify_pre_aggregated(&message, &rev_signing_agg_pub)
            );

            /*
             * The full set of signed keys aggregated in non-sequential
             * order should pass verification.
             *
             * Note: "shuffled" is used loosely here: we split the vec of keys in half, put
             * the last half in front of the first half and then swap the first and last elements.
             */
            let mut shuffled_signing_agg_pub = AggregatePublicKey::new();
            let n = signing_kps.len();
            assert!(
                n > 2,
                "test error: shuffle is ineffective with less than two elements"
            );
            let mut order: Vec<usize> = ((n / 2)..n).collect();
            order.append(&mut (0..(n / 2)).collect());
            order.swap(0, n - 1);
            for i in order {
                shuffled_signing_agg_pub.add(&signing_kps[i].pk);
            }
            assert!(agg_signature
                .fast_aggregate_verify_pre_aggregated(&message, &shuffled_signing_agg_pub));

            /*
             * The signature should fail if an signing key has double-signed the
             * aggregate signature.
             */
            let mut double_sig_agg_sig = agg_signature.clone();
            let extra_sig = Signature::new(&message, &signing_kps[0].sk);
            double_sig_agg_sig.add(&extra_sig);
            assert!(!double_sig_agg_sig
                .fast_aggregate_verify_pre_aggregated(&message, &signing_agg_pub));

            /*
             * The full set of signed keys should fail verification if one key signs across a
             * different message.
             */
            let mut distinct_msg_agg_sig = AggregateSignature::new();
            let mut distinct_msg_agg_pub = AggregatePublicKey::new();
            for (i, kp) in signing_kps.iter().enumerate() {
                let message = match i {
                    0 => "different_msg!1".as_bytes(),
                    _ => message,
                };
                let sig = Signature::new(&message, &kp.sk);
                distinct_msg_agg_sig.add(&sig);
                distinct_msg_agg_pub.add(&kp.pk);
            }
            assert!(!distinct_msg_agg_sig
                .fast_aggregate_verify_pre_aggregated(&message, &distinct_msg_agg_pub));

            /*
             * The signature should fail if an extra, non-signing key has signed the
             * aggregate signature.
             */
            let mut super_set_agg_sig = agg_signature.clone();
            let extra_sig = Signature::new(&message, &non_signing_kps[0].sk);
            super_set_agg_sig.add(&extra_sig);
            assert!(
                !super_set_agg_sig.fast_aggregate_verify_pre_aggregated(&message, &signing_agg_pub)
            );

            /*
             * A subset of signed keys should fail verification.
             */
            let mut subset_pub_keys: Vec<&PublicKey> =
                signing_kps_subset.iter().map(|kp| &kp.pk).collect();
            let subset_agg_key = AggregatePublicKey::from_public_keys(&subset_pub_keys.as_slice());
            assert!(!agg_signature.fast_aggregate_verify_pre_aggregated(&message, &subset_agg_key));
            // Sanity check the subset test by completing the set and verifying it.
            subset_pub_keys.push(&signing_kps[signing_kps.len() - 1].pk);
            let subset_agg_key = AggregatePublicKey::from_public_keys(&subset_pub_keys);
            assert!(agg_signature.fast_aggregate_verify_pre_aggregated(&message, &subset_agg_key));

            /*
             * A set of keys which did not sign the message at all should fail
             */
            let non_signing_pub_keys: Vec<&PublicKey> =
                non_signing_kps.iter().map(|kp| &kp.pk).collect();
            let non_signing_agg_key =
                AggregatePublicKey::from_public_keys(&non_signing_pub_keys.as_slice());
            assert!(
                !agg_signature.fast_aggregate_verify_pre_aggregated(&message, &non_signing_agg_key)
            );

            /*
             * An empty aggregate pub key (it has not had any keys added to it) should
             * fail.
             */
            let empty_agg_pub = AggregatePublicKey::new();
            assert!(!agg_signature.fast_aggregate_verify_pre_aggregated(&message, &empty_agg_pub));
        }
    }

    #[test]
    fn test_random_aggregate_public_keys() {
        let control_kp = Keypair::random(&mut rand::thread_rng());
        let signing_kps = vec![
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
        ];
        let non_signing_kps = vec![
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
        ];
        helper_test_aggregate_public_keys(control_kp, signing_kps, non_signing_kps);
    }

    #[test]
    fn test_known_aggregate_public_keys() {
        let control_secret_key_bytes = vec![vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 40, 129, 16, 229, 203, 159, 171, 37,
            94, 38, 3, 24, 17, 213, 243, 246, 122, 105, 202, 156, 186, 237, 54, 148, 116, 130, 20,
            138, 15, 134, 45, 73,
        ]];
        let control_kps = map_secret_bytes_to_keypairs(control_secret_key_bytes);
        let control_kp = control_kps[0].clone();
        let signing_secret_key_bytes = vec![
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 98, 161, 50, 32, 254, 87, 16, 25,
                167, 79, 192, 116, 176, 74, 164, 217, 40, 57, 179, 15, 19, 21, 240, 100, 70, 127,
                111, 170, 129, 137, 42, 53,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 53, 72, 211, 104, 184, 68, 142,
                208, 115, 22, 156, 97, 28, 216, 228, 102, 4, 218, 116, 226, 166, 131, 67, 7, 40,
                55, 157, 167, 157, 127, 143, 13,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 94, 157, 163, 128, 239, 119, 116,
                194, 162, 172, 189, 100, 36, 33, 13, 31, 137, 177, 80, 73, 119, 126, 246, 215, 123,
                178, 195, 12, 141, 65, 65, 89,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 74, 195, 255, 195, 62, 36, 197, 48,
                100, 25, 121, 8, 191, 219, 73, 136, 227, 203, 98, 123, 204, 27, 197, 66, 193, 107,
                115, 53, 5, 98, 137, 77,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 82, 16, 65, 222, 228, 32, 47, 1,
                245, 135, 169, 125, 46, 120, 57, 149, 121, 254, 168, 52, 30, 221, 150, 186, 157,
                141, 25, 143, 175, 196, 21, 176,
            ],
        ];
        let signing_kps = map_secret_bytes_to_keypairs(signing_secret_key_bytes);
        let non_signing_secret_key_bytes = vec![
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 235, 126, 159, 58, 82, 170, 175,
                73, 188, 251, 60, 79, 24, 164, 146, 88, 210, 177, 65, 62, 183, 124, 129, 109, 248,
                181, 29, 16, 128, 207, 23,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 177, 235, 229, 217, 215, 204,
                237, 178, 196, 182, 51, 28, 147, 58, 24, 79, 134, 41, 185, 153, 133, 229, 195, 32,
                221, 247, 171, 91, 196, 65, 250,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 65, 154, 236, 86, 178, 14, 179,
                117, 113, 4, 40, 173, 150, 221, 23, 7, 117, 162, 173, 104, 172, 241, 111, 31, 170,
                241, 185, 31, 69, 164, 115, 126,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 13, 67, 192, 157, 69, 188, 53, 161,
                77, 187, 133, 49, 254, 165, 47, 189, 185, 150, 23, 231, 143, 31, 64, 208, 134, 147,
                53, 53, 228, 225, 104, 62,
            ],
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 22, 66, 26, 11, 101, 38, 37, 1,
                148, 156, 162, 211, 37, 231, 37, 222, 172, 36, 224, 218, 187, 127, 122, 195, 229,
                234, 124, 91, 246, 73, 12, 120,
            ],
        ];
        let non_signing_kps = map_secret_bytes_to_keypairs(non_signing_secret_key_bytes);
        helper_test_aggregate_public_keys(control_kp, signing_kps, non_signing_kps);
    }

    #[test]
    pub fn add_aggregate_public_key() {
        let keypair_1 = Keypair::random(&mut rand::thread_rng());
        let keypair_2 = Keypair::random(&mut rand::thread_rng());
        let keypair_3 = Keypair::random(&mut rand::thread_rng());
        let keypair_4 = Keypair::random(&mut rand::thread_rng());

        let aggregate_public_key12 =
            AggregatePublicKey::from_public_keys(&[&keypair_1.pk, &keypair_2.pk]);

        let aggregate_public_key34 =
            AggregatePublicKey::from_public_keys(&[&keypair_3.pk, &keypair_4.pk]);

        // Should be the same as adding two aggregates
        let aggregate_public_key1234 = AggregatePublicKey::from_public_keys(&[
            &keypair_1.pk,
            &keypair_2.pk,
            &keypair_3.pk,
            &keypair_4.pk,
        ]);

        // Aggregate AggregatePublicKeys
        let mut add_aggregate_public_key = AggregatePublicKey::new();
        add_aggregate_public_key.add_aggregate(&aggregate_public_key12);
        add_aggregate_public_key.add_aggregate(&aggregate_public_key34);

        assert_eq!(add_aggregate_public_key, aggregate_public_key1234);
    }

    #[test]
    pub fn add_aggregate_signature() {
        let msg: Vec<u8> = vec![1; 32];

        let keypair_1 = Keypair::random(&mut rand::thread_rng());
        let keypair_2 = Keypair::random(&mut rand::thread_rng());
        let keypair_3 = Keypair::random(&mut rand::thread_rng());
        let keypair_4 = Keypair::random(&mut rand::thread_rng());

        let sig_1 = Signature::new(&msg, &keypair_1.sk);
        let sig_2 = Signature::new(&msg, &keypair_2.sk);
        let sig_3 = Signature::new(&msg, &keypair_3.sk);
        let sig_4 = Signature::new(&msg, &keypair_4.sk);

        // Should be the same as adding two aggregates
        let aggregate_public_key = AggregatePublicKey::from_public_keys(&[
            &keypair_1.pk,
            &keypair_2.pk,
            &keypair_3.pk,
            &keypair_4.pk,
        ]);

        let mut aggregate_signature = AggregateSignature::new();
        aggregate_signature.add(&sig_1);
        aggregate_signature.add(&sig_2);
        aggregate_signature.add(&sig_3);
        aggregate_signature.add(&sig_4);

        let mut add_aggregate_signature = AggregateSignature::new();
        add_aggregate_signature.add(&sig_1);
        add_aggregate_signature.add(&sig_2);

        let mut aggregate_signature34 = AggregateSignature::new();
        aggregate_signature34.add(&sig_3);
        aggregate_signature34.add(&sig_4);

        add_aggregate_signature.add_aggregate(&aggregate_signature34);

        add_aggregate_signature.point.affine();
        aggregate_signature.point.affine();

        assert_eq!(add_aggregate_signature, aggregate_signature);
        assert!(add_aggregate_signature
            .fast_aggregate_verify_pre_aggregated(&msg, &aggregate_public_key));
    }

    #[test]
    pub fn test_verify_multiple_signatures() {
        let mut rng = &mut rand::thread_rng();
        let n = 10; // Signatures
        let m = 3; // PublicKeys per Signature
        let mut msgs: Vec<Vec<u8>> = vec![vec![]; n];
        let mut public_keys: Vec<Vec<PublicKey>> = vec![vec![]; n];
        let mut aggregate_signatures: Vec<AggregateSignature> = vec![];

        let keypairs: Vec<Keypair> = (0..n * m).map(|_| Keypair::random(&mut rng)).collect();

        for i in 0..n {
            let mut aggregate_signature = AggregateSignature::new();
            msgs[i] = vec![i as u8; 32];
            for j in 0..m {
                let keypair = &keypairs[i * m + j];
                public_keys[i].push(keypair.pk.clone());

                let signature = Signature::new(&msgs[i], &keypair.sk);
                aggregate_signature.add(&signature);
            }
            aggregate_signatures.push(aggregate_signature);
        }

        // Remove mutability
        let msgs: Vec<Vec<u8>> = msgs;
        let public_keys: Vec<Vec<PublicKey>> = public_keys;
        let aggregate_signatures: Vec<AggregateSignature> = aggregate_signatures;

        // Create reference iterators
        let ref_vec = vec![1u8; 32];
        let ref_pk = PublicKey::new_from_raw(&GroupG1::new());
        let ref_as = AggregateSignature::new();
        let mut msgs_refs: Vec<&[u8]> = vec![&ref_vec; n];
        let mut public_keys_refs: Vec<Vec<&PublicKey>> = vec![vec![&ref_pk; m]; n];
        let mut aggregate_signatures_refs: Vec<&AggregateSignature> = vec![&ref_as; n];

        for i in 0..n {
            msgs_refs[i] = &msgs[i];
            aggregate_signatures_refs[i] = &aggregate_signatures[i];
            for j in 0..m {
                public_keys_refs[i][j] = &public_keys[i][j];
            }
        }

        let mega_iter = aggregate_signatures_refs
            .into_iter()
            .zip(public_keys_refs.iter().map(|x| x.as_slice()))
            .zip(msgs_refs.iter().map(|x| *x))
            .map(|((a, b), c)| (a, b, c));

        let valid = AggregateSignature::verify_multiple_aggregate_signatures(&mut rng, mega_iter);

        assert!(valid);
    }
}
