use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Price = Decimal;
pub type Amount = Decimal;
pub type OrderId = Uuid;
pub type TradeId = Uuid;
pub type UserId = Uuid;
pub type MarketId = String; // e.g BTC-USD

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub user_id: UserId,
    pub market_id: MarketId,
    pub side: Side,
    pub order_type: OrderType,
    pub amount: Amount,
    pub filled_amount: Amount,
    pub price: Option<Price>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: TradeId,
    pub market_id: MarketId,
    pub taker_order_id: OrderId,
    pub maker_order_id: OrderId,
    pub amount: Amount,
    pub price: Price,
    pub timestamp: i64,
    pub taker_side: Side,
}
