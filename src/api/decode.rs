use std::str::FromStr;

use axum::Json;
use ldk_node::lightning::offers::offer::{Amount, Offer};
use ldk_node::lightning_invoice::Bolt11Invoice;

use crate::api::error::AppError;
use crate::types::{
    DecodeInvoiceRequest, DecodeInvoiceResponse, DecodeOfferRequest, DecodeOfferResponse,
    RoutingHint, RoutingHintHop,
};

pub fn handle_decode_invoice(
    req: &DecodeInvoiceRequest,
) -> Result<Json<DecodeInvoiceResponse>, AppError> {
    let invoice = Bolt11Invoice::from_str(&req.invoice)
        .map_err(|e| AppError::BadRequest(format!("Invalid BOLT11 invoice: {e}")))?;

    let amount_msat = invoice.amount_milli_satoshis();
    let amount_sat = amount_msat.map(|m| m / 1000);
    let payment_hash = invoice.payment_hash().to_string();
    let payment_secret = hex::DisplayHex::to_lower_hex_string(&invoice.payment_secret().0[..]);

    let description = Some(invoice.description().to_string());

    let expiry_seconds = invoice.expiry_time().as_secs();
    let created_at_seconds = invoice
        .timestamp()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let node_id = invoice.recover_payee_pub_key().to_string();

    let routing_hints: Vec<RoutingHint> = invoice
        .route_hints()
        .into_iter()
        .map(|hint| RoutingHint {
            hops: hint
                .0
                .into_iter()
                .map(|hop| RoutingHintHop {
                    node_id: hop.src_node_id.to_string(),
                    short_channel_id: hop.short_channel_id.to_string(),
                    fee_base_msat: hop.fees.base_msat,
                    fee_proportional_millionths: hop.fees.proportional_millionths,
                    cltv_expiry_delta: hop.cltv_expiry_delta,
                })
                .collect(),
        })
        .collect();

    Ok(Json(DecodeInvoiceResponse {
        amount: amount_sat,
        amount_msat,
        payment_hash,
        payment_secret,
        description,
        payment_metadata: None,
        expiry_seconds,
        created_at_seconds,
        node_id,
        routing_hints,
        features: vec![],
    }))
}

pub fn handle_decode_offer(
    req: &DecodeOfferRequest,
) -> Result<Json<DecodeOfferResponse>, AppError> {
    let offer = Offer::from_str(&req.offer)
        .map_err(|e| AppError::BadRequest(format!("Invalid BOLT12 offer: {e:?}")))?;

    let offer_id = hex::DisplayHex::to_lower_hex_string(&offer.id().0[..]);

    let (amount_msat, amount_sat) = match offer.amount() {
        Some(Amount::Bitcoin { amount_msats }) => (Some(amount_msats), Some(amount_msats / 1000)),
        _ => (None, None),
    };

    let description = offer.description().map(|d| d.to_string());
    let issuer = offer.issuer().map(|i| i.to_string());
    let node_id = offer.issuer_signing_pubkey().map(|k| k.to_string());

    Ok(Json(DecodeOfferResponse {
        offer_id,
        amount: amount_sat,
        amount_msat,
        description,
        issuer,
        node_id,
        features: vec![],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ldk_node::bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    use ldk_node::lightning::offers::offer::OfferBuilder;

    use crate::types::{DecodeInvoiceRequest, DecodeOfferRequest};

    // Signet invoice with LSPS4 JIT route hint (single hop), "1 cup coffee".
    // {
    //   "amount": 250000,
    //   "amountMsat": 250000000,
    //   "paymentHash": "417fea9ae55c0ecec4060ab19e366f58b3460af1504be8c2b694a051cde90cea",
    //   "paymentSecret": "6d0d543361f5931e058ba447060051c54763824c251c752a3efc2f5d445deb1c",
    //   "description": "1 cup coffee",
    //   "expirySeconds": 3600,
    //   "createdAtSeconds": 1774391559,
    //   "nodeId": "02750f8964944768b167751f9114819eecb47e915bdc3b326495733a8e27a02878",
    //   "routingHints": [{ "hops": [{
    //     "nodeId": "03fd9a377576df94cc7e458471c43c400630655083dee89df66c6ad38d1b7acffd",
    //     "shortChannelId": "3248629150142627842",
    //     "feeBaseMsat": 0, "feeProportionalMillionths": 0, "cltvExpiryDelta": 72
    //   }]}]
    // }
    const BOLT11: &str = "lntbs2500u1p5uxyg8dq5xysxxatsyp3k7enxv4jspp5g9l74xh9ts8va3qxp2ceudn0tze5vzh32p973s4kjjs9rn0fpn4qsp5d5x4gvmp7kf3upvt53rsvqz3c4rk8qjvy5w82237lsh463zaavwq9qyysgqcqzpvxqrrssrzjq07e5dm4wm0efnr7gkz8r3pugqrrqe2ss00w380kd34d8rgm0t8l6tg4wvqq2wcqqgqqqqqqqqqqqqqqfql6cwtcuver326t0zm77hugnskzn06hza4rrfej5dl7pg3jzuvs4j453vrwzx38n2ylmpyrxkc9ssw55r2008fd87y38jr6h4akpkzccpydcc37";

    fn decode(invoice: &str) -> DecodeInvoiceResponse {
        let req = DecodeInvoiceRequest {
            invoice: invoice.to_string(),
        };
        handle_decode_invoice(&req).unwrap().0
    }

    #[test]
    fn decodes_fields() {
        let resp = decode(BOLT11);
        assert_eq!(resp.amount, Some(250_000));
        assert_eq!(resp.amount_msat, Some(250_000_000));
        assert_eq!(
            resp.payment_hash,
            "417fea9ae55c0ecec4060ab19e366f58b3460af1504be8c2b694a051cde90cea"
        );
        assert_eq!(
            resp.payment_secret,
            "6d0d543361f5931e058ba447060051c54763824c251c752a3efc2f5d445deb1c"
        );
        assert_eq!(resp.description.as_deref(), Some("1 cup coffee"));
        assert_eq!(resp.expiry_seconds, 3600);
        assert_eq!(resp.created_at_seconds, 1774391559);
        assert_eq!(
            resp.node_id,
            "02750f8964944768b167751f9114819eecb47e915bdc3b326495733a8e27a02878"
        );
    }

    #[test]
    fn decodes_routing_hints() {
        let resp = decode(BOLT11);
        assert_eq!(resp.routing_hints.len(), 1);

        let hint = &resp.routing_hints[0];
        assert_eq!(hint.hops.len(), 1);

        let hop = &hint.hops[0];
        assert_eq!(
            hop.node_id,
            "03fd9a377576df94cc7e458471c43c400630655083dee89df66c6ad38d1b7acffd"
        );
        assert_eq!(hop.short_channel_id, "3248629150142627842");
        assert_eq!(hop.fee_base_msat, 0);
        assert_eq!(hop.fee_proportional_millionths, 0);
        assert_eq!(hop.cltv_expiry_delta, 72);
    }

    #[test]
    fn rejects_garbage() {
        let req = DecodeInvoiceRequest {
            invoice: "garbage".to_string(),
        };
        let err = handle_decode_invoice(&req).unwrap_err();
        match err {
            AppError::BadRequest(msg) => assert!(msg.contains("Invalid BOLT11")),
            other => panic!("Expected BadRequest, got: {:?}", other),
        }
    }

    #[test]
    fn rejects_empty_string() {
        let req = DecodeInvoiceRequest {
            invoice: String::new(),
        };
        assert!(handle_decode_invoice(&req).is_err());
    }

    fn decode_offer(offer: &str) -> DecodeOfferResponse {
        let req = DecodeOfferRequest {
            offer: offer.to_string(),
        };
        handle_decode_offer(&req).unwrap().0
    }

    fn build_test_offer() -> String {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[42u8; 32]).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);

        OfferBuilder::new(pubkey)
            .amount_msats(250_000_000)
            .description("1 cup coffee".to_string())
            .issuer("mdkd test".to_string())
            .build()
            .unwrap()
            .to_string()
    }

    #[test]
    fn offer_decodes_all_fields() {
        let offer_str = build_test_offer();
        let resp = decode_offer(&offer_str);

        assert_eq!(resp.offer_id.len(), 64);
        assert_eq!(resp.amount_msat, Some(250_000_000));
        assert_eq!(resp.amount, Some(250_000));
        assert_eq!(resp.description.as_deref(), Some("1 cup coffee"));
        assert_eq!(resp.issuer.as_deref(), Some("mdkd test"));
        assert!(resp.node_id.is_some());
        assert_eq!(resp.node_id.as_deref().unwrap().len(), 66);
    }

    #[test]
    fn offer_rejects_garbage() {
        let req = DecodeOfferRequest {
            offer: "garbage".to_string(),
        };
        let err = handle_decode_offer(&req).unwrap_err();
        match err {
            AppError::BadRequest(msg) => assert!(msg.contains("Invalid BOLT12")),
            other => panic!("Expected BadRequest, got: {:?}", other),
        }
    }

    #[test]
    fn offer_rejects_bolt11() {
        let req = DecodeOfferRequest {
            offer: BOLT11.to_string(),
        };
        assert!(handle_decode_offer(&req).is_err());
    }
}
