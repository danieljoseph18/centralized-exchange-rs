use core_types::{Amount, Order, OrderId, OrderType, Price, Side, Trade};
use rust_decimal::Decimal;
/**
 * - 2 separate b trees -> 1 bid, 1 ask
 * - functions to add, remove, update orders
 * - functions to match orders
 */
use std::collections::{BTreeMap, HashMap, VecDeque};
use uuid::Uuid;

// Result type for match_order function
#[derive(Debug)]
pub struct MatchResult {
    pub trades: Vec<Trade>,
    // The remaining part of the taker order if it wasn't fully filled
    // None if the order was fully filled or if it was a market order
    // that couldn't be filled at all.
    pub remaining_taker_order: Option<Order>,
    // Orders that were partially or fully filled and removed from the book
    pub removed_maker_orders: Vec<OrderId>,
    // Orders that were partially filled but remain in the book
    pub updated_maker_orders: Vec<Order>,
}

#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub bids: Vec<OrderBookLevel>, // Highest bid first
    pub asks: Vec<OrderBookLevel>, // Lowest ask first
}

#[derive(Debug, Clone)]
pub struct OrderBookLevel {
    pub price: Price,
    pub amount: Amount, // Aggregated amount at this price level
}

pub struct OrderBook {
    // Bids ordered price highest to lowest
    bids: BTreeMap<Price, VecDeque<Order>>,
    // Asks ordered price lowest to highest
    asks: BTreeMap<Price, VecDeque<Order>>,
    // Fast lookup for order price by order id
    order_locations: HashMap<OrderId, Price>,
}

impl OrderBook {
    pub fn new() -> Self {
        OrderBook {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_locations: HashMap::new(),
        }
    }

    pub fn add_bid(&mut self, order: Order) {
        // Checks
        assert_eq!(order.side, Side::Buy);

        assert_eq!(order.order_type, OrderType::Limit);

        let price = order.price.expect("Limit orders must have a price");

        self.order_locations.insert(order.id, price);

        self.bids
            .entry(price)
            .or_insert_with(VecDeque::new)
            .push_back(order);
    }

    pub fn add_ask(&mut self, order: Order) {
        assert_eq!(order.side, Side::Sell);

        assert_eq!(order.order_type, OrderType::Limit);

        let price = order.price.expect("Limit orders must have a price");

        self.order_locations.insert(order.id, price);

        self.asks
            .entry(price)
            .or_insert_with(VecDeque::new)
            .push_back(order);
    }

    pub fn remove_bid(&mut self, order_id: OrderId) -> Option<Order> {
        self.remove_order(order_id, Side::Buy)
    }

    pub fn remove_ask(&mut self, order_id: OrderId) -> Option<Order> {
        self.remove_order(order_id, Side::Sell)
    }

    fn remove_order(&mut self, order_id: OrderId, side: Side) -> Option<Order> {
        let price = match self.order_locations.get(&order_id) {
            Some(p) => *p,
            None => return None,
        };

        let book_side = match side {
            Side::Buy => self.bids.get_mut(&price),
            Side::Sell => self.asks.get_mut(&price),
        };

        if let Some(orders_at_price) = book_side {
            if let Some(index) = orders_at_price.iter().position(|o| o.id == order_id) {
                // Remove order from the VecDeque
                let removed_order = orders_at_price.remove(index).unwrap();

                // If VecDeque empty, remove the price level from BTreeMap
                if orders_at_price.is_empty() {
                    match side {
                        Side::Buy => self.bids.remove(&price),
                        Side::Sell => self.asks.remove(&price),
                    };
                }

                self.order_locations.remove(&order_id);

                return Some(removed_order);
            }
        }

        self.order_locations.remove(&order_id);
        None
    }

    pub fn update_bid_amount(
        &mut self,
        order_id: OrderId,
        new_amount: Amount,
    ) -> Result<(), String> {
        self.update_order_amount(order_id, new_amount, Side::Buy)
    }

    pub fn update_ask_amount(
        &mut self,
        order_id: OrderId,
        new_amount: Amount,
    ) -> Result<(), String> {
        self.update_order_amount(order_id, new_amount, Side::Sell)
    }

    fn update_order_amount(
        &mut self,
        order_id: OrderId,
        new_amount: Amount,
        side: Side,
    ) -> Result<(), String> {
        let price = match self.order_locations.get(&order_id) {
            Some(p) => *p,
            None => return Err("Order ID not found".to_string()),
        };

        let book_side = match side {
            Side::Buy => self.bids.get_mut(&price),
            Side::Sell => self.asks.get_mut(&price),
        };

        if let Some(orders_at_price) = book_side {
            if let Some(order) = orders_at_price.iter_mut().find(|o| o.id == order_id) {
                if new_amount <= Decimal::ZERO || new_amount > order.amount {
                    return Err(format!(
                        "Invalid new amount: {}. Must be > 0 and <= original amount {}",
                        new_amount, order.amount
                    ));
                }

                if new_amount < order.filled_amount {
                    return Err(format!(
                        "Invalid new amount: {}. Cannot be less than already filled amount {}",
                        new_amount, order.filled_amount
                    ));
                }

                order.amount = new_amount;

                return Ok(());
            }
        }

        Err(format!(
            "Order {} not found at price level {}",
            order_id, price
        ))
    }

    /// For market orders --> execute the order immediately
    /// Matches an incoming (taker) order against the existing orders (maker) in the book.
    /// Handles both Limit and Market orders.
    pub fn match_order(&mut self, mut taker_order: Order) -> MatchResult {
        let mut trades = Vec::new();
        let mut removed_maker_orders = Vec::new();
        let mut updated_maker_orders = Vec::new();
        let mut remaining_taker_order = None;

        let taker_unfilled_amount = taker_order.amount - taker_order.filled_amount;

        if taker_unfilled_amount <= Decimal::ZERO {
            // Nothing to match if the order is already filled or has zero amount
            return MatchResult {
                trades,
                remaining_taker_order: Some(taker_order), // Return original order state
                removed_maker_orders,
                updated_maker_orders,
            };
        }

        match taker_order.side {
            // Taker is BUYING: Match against ASKS (lowest price first)
            Side::Buy => {
                let mut ask_prices: Vec<Price> = self.asks.keys().cloned().collect();
                ask_prices.sort(); // Ensure ascending order (BTreeMap default)

                for price in ask_prices {
                    // Check if taker is still willing to buy at this price
                    match taker_order.order_type {
                        OrderType::Limit => {
                            if price > taker_order.price.unwrap() {
                                break; // Stop if ask price is higher than limit price
                            }
                        }
                        OrderType::Market => {
                            // Market order takes any price
                        }
                    }

                    if let Some(orders_at_price) = self.asks.get_mut(&price) {
                        let mut orders_fully_filled = Vec::new(); // Indices to remove

                        for (index, maker_order) in orders_at_price.iter_mut().enumerate() {
                            let taker_remaining = taker_order.amount - taker_order.filled_amount;
                            if taker_remaining <= Decimal::ZERO {
                                break; // Taker order fully filled
                            }

                            let maker_remaining = maker_order.amount - maker_order.filled_amount;
                            let fill_amount = taker_remaining.min(maker_remaining);

                            if fill_amount <= Decimal::ZERO {
                                continue; // Maker order already filled? Skip.
                            }

                            // Update filled amounts
                            taker_order.filled_amount += fill_amount;
                            maker_order.filled_amount += fill_amount;

                            // Create Trade
                            let trade = Trade {
                                id: Uuid::new_v4(), // Generate unique trade ID
                                market_id: taker_order.market_id.clone(),
                                taker_order_id: taker_order.id,
                                maker_order_id: maker_order.id,
                                amount: fill_amount,
                                price, // Trade occurs at the maker's price
                                timestamp: chrono::Utc::now().timestamp_millis(),
                                taker_side: Side::Buy,
                            };
                            trades.push(trade);

                            // Check if maker order is fully filled
                            if maker_order.filled_amount >= maker_order.amount {
                                orders_fully_filled.push(index);
                                removed_maker_orders.push(maker_order.id);
                                self.order_locations.remove(&maker_order.id);
                            } else {
                                // Add to updated list if partially filled
                                updated_maker_orders.push(maker_order.clone());
                            }
                        } // End loop through orders at this price level

                        // Remove fully filled maker orders from VecDeque (in reverse index order)
                        for index in orders_fully_filled.into_iter().rev() {
                            orders_at_price.remove(index);
                        }

                        // If VecDeque is empty, remove the price level
                        if orders_at_price.is_empty() {
                            self.asks.remove(&price);
                        }
                    } // End if let Some(orders_at_price)

                    if taker_order.filled_amount >= taker_order.amount {
                        break; // Taker order fully filled
                    }
                } // End loop through ask prices
            }

            // Taker is SELLING: Match against BIDS (highest price first)
            Side::Sell => {
                // Need to iterate bids in descending price order
                let mut bid_prices: Vec<Price> = self.bids.keys().cloned().collect();
                bid_prices.sort_by(|a, b| b.cmp(a)); // Sort descending

                for price in bid_prices {
                    // Check if taker is still willing to sell at this price
                    match taker_order.order_type {
                        OrderType::Limit => {
                            if price < taker_order.price.unwrap() {
                                break; // Stop if bid price is lower than limit price
                            }
                        }
                        OrderType::Market => {
                            // Market order takes any price
                        }
                    }

                    if let Some(orders_at_price) = self.bids.get_mut(&price) {
                        let mut orders_fully_filled = Vec::new(); // Indices to remove

                        for (index, maker_order) in orders_at_price.iter_mut().enumerate() {
                            let taker_remaining = taker_order.amount - taker_order.filled_amount;
                            if taker_remaining <= Decimal::ZERO {
                                break; // Taker order fully filled
                            }

                            let maker_remaining = maker_order.amount - maker_order.filled_amount;
                            let fill_amount = taker_remaining.min(maker_remaining);

                            if fill_amount <= Decimal::ZERO {
                                continue;
                            }

                            // Update filled amounts
                            taker_order.filled_amount += fill_amount;
                            maker_order.filled_amount += fill_amount;

                            // Create Trade
                            let trade = Trade {
                                id: Uuid::new_v4(),
                                market_id: taker_order.market_id.clone(),
                                taker_order_id: taker_order.id,
                                maker_order_id: maker_order.id,
                                amount: fill_amount,
                                price, // Trade occurs at the maker's price
                                timestamp: chrono::Utc::now().timestamp_millis(),
                                taker_side: Side::Sell,
                            };
                            trades.push(trade);

                            // Check if maker order is fully filled
                            if maker_order.filled_amount >= maker_order.amount {
                                orders_fully_filled.push(index);
                                removed_maker_orders.push(maker_order.id);
                                self.order_locations.remove(&maker_order.id);
                            } else {
                                updated_maker_orders.push(maker_order.clone());
                            }
                        } // End loop through orders at this price level

                        // Remove fully filled maker orders
                        for index in orders_fully_filled.into_iter().rev() {
                            orders_at_price.remove(index);
                        }

                        // If VecDeque is empty, remove the price level
                        if orders_at_price.is_empty() {
                            self.bids.remove(&price);
                        }
                    } // End if let Some(orders_at_price)

                    if taker_order.filled_amount >= taker_order.amount {
                        break; // Taker order fully filled
                    }
                } // End loop through bid prices
            }
        }

        // If taker order is a Limit order and not fully filled, store the remainder
        if taker_order.order_type == OrderType::Limit
            && taker_order.filled_amount < taker_order.amount
        {
            remaining_taker_order = Some(taker_order);
        } else if taker_order.order_type == OrderType::Market
            && taker_order.filled_amount < taker_order.amount
        {
            // Market order couldn't be fully filled, it just expires partially filled
            // We don't set remaining_taker_order, signaling it's done.
            // The caller can inspect taker_order.filled_amount if needed.
        }

        MatchResult {
            trades,
            remaining_taker_order,
            removed_maker_orders,
            updated_maker_orders,
        }
    }

    /// Get's the highest bid price in the orderbook
    /// None if no bids in the book
    pub fn get_best_bid(&self) -> Option<Price> {
        self.bids.keys().last().cloned()
    }

    /// Gets the lowest ask price in the orderbook
    pub fn get_best_ask(&self) -> Option<Price> {
        self.asks.keys().next().cloned()
    }

    pub fn get_order_book_snapshot(&self) -> OrderBookSnapshot {
        let bids = self
            .bids
            .iter()
            .rev() // Iterate highest price first
            .map(|(price, orders)| OrderBookLevel {
                price: *price,
                amount: orders
                    .iter()
                    .map(|o| o.amount - o.filled_amount) // Sum unfilled amounts
                    .sum(),
            })
            .collect();

        let asks = self
            .asks
            .iter() // Iterate lowest price first
            .map(|(price, orders)| OrderBookLevel {
                price: *price,
                amount: orders
                    .iter()
                    .map(|o| o.amount - o.filled_amount) // Sum unfilled amounts
                    .sum(),
            })
            .collect();

        OrderBookSnapshot { bids, asks }
    }

    pub fn get_order_by_id(&self, order_id: &OrderId) -> Option<Order> {
        if let Some(price) = self.order_locations.get(order_id) {
            // Check bids
            if let Some(orders_at_price) = self.bids.get(price) {
                if let Some(order) = orders_at_price.iter().find(|o| o.id == *order_id) {
                    return Some(order.clone());
                }
            }

            // Check asks
            if let Some(orders_at_price) = self.asks.get(price) {
                if let Some(order) = orders_at_price.iter().find(|o| o.id == *order_id) {
                    return Some(order.clone());
                }
            }
        }

        //  If not found in order_locations or BTreeMaps
        None
    }
}
