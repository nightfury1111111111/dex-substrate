use support::{decl_module, decl_storage, decl_event, StorageValue, StorageMap, dispatch::Result, Parameter, ensure};
use runtime_primitives::traits::{Member, Bounded, SimpleArithmetic, Hash, As, Zero, CheckedSub};
use system::ensure_signed;
use parity_codec::{Encode, Decode};
use rstd::{result, ops::Not};
use crate::token;

use primitives::U256;
use core::convert::TryInto;

pub trait Trait: token::Trait + system::Trait {
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
	type Price: Parameter + Member + Default + Bounded + SimpleArithmetic + Copy + From<u64> + Into<u64>;
}

#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq)]
pub struct TradePair<Hash> {
	hash: Hash,
	base: Hash,
	quote: Hash,
}

#[derive(Debug, Encode, Decode, Clone, PartialEq, Copy)]
pub enum OrderType {
	Buy,
	Sell,
}

impl Not for OrderType {
	type Output = OrderType;

	fn not(self) -> Self::Output {
		match self {
			OrderType::Sell => OrderType::Buy,
			OrderType::Buy => OrderType::Sell,
		}
	}
}

#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy)]
pub enum OrderStatus {
	Created,
	PartialFilled,
	Filled,
	Canceled,
}

#[derive(Encode, Decode, Clone, PartialEq, Debug)]
pub struct Trade<T> where T: Trait {
	hash: T::Hash,
	base: T::Hash,
	quote: T::Hash,
	buyer: T::AccountId, // have base
	seller: T::AccountId, // have quote
	maker: T::AccountId, // list order first
	taker: T::AccountId, // list order second
	otype: OrderType, // taker's order type
	price: T::Price, // maker's order price
	base_amount: T::Balance,
	quote_amount: T::Balance,
}

#[derive(Encode, Decode, Clone)]
pub struct LimitOrder<T> where T: Trait {
	hash: T::Hash,
	base: T::Hash,
	quote: T::Hash,
	owner: T::AccountId,
	price: T::Price,
	sell_amount: T::Balance,
	remained_sell_amount: T::Balance,
	buy_amount: T::Balance,
	remained_buy_amount: T::Balance,
	otype: OrderType,
	status: OrderStatus,
}

impl<T> LimitOrder<T> where T: Trait {
	pub fn new(base: T::Hash,
		quote: T::Hash,
		owner: T::AccountId,
		price: T::Price,
		sell_amount: T::Balance,
		buy_amount: T::Balance,
		otype: OrderType) 
		-> Self {
		let hash = (base, quote, price, sell_amount, buy_amount, owner.clone(), <system::Module<T>>::random_seed()).using_encoded(<T as system::Trait>::Hashing::hash);

		LimitOrder {
			hash, base, quote, owner, price, otype,
			sell_amount, buy_amount,
			status: OrderStatus::Created,
			remained_sell_amount: sell_amount,
			remained_buy_amount: buy_amount,
		}
	}

	fn is_finished(&self) -> bool {
		(self.remained_buy_amount == Zero::zero() && self.status == OrderStatus::Filled)
		|| self.status == OrderStatus::Canceled
	}
}

impl<T> Trade<T> where T: Trait {
	fn new(base: T::Hash, quote: T::Hash, maker_order: &LimitOrder<T>, taker_order: &LimitOrder<T>, base_amount: T::Balance, quote_amount: T::Balance) -> Self {
		let nonce = <Nonce<T>>::get();

		let hash = (base, quote, base_amount, quote_amount, nonce,<system::Module<T>>::random_seed()).using_encoded(<T as system::Trait>::Hashing::hash);

		<Nonce<T>>::mutate(|x| *x += 1);

		let buyer;
		let seller;
		if taker_order.otype == OrderType::Buy {
			buyer = taker_order.owner.clone();
			seller = maker_order.owner.clone();
		} else {
			seller = taker_order.owner.clone();
			buyer = maker_order.owner.clone();
		}

		Trade {
			hash, base, quote, buyer, seller, base_amount, quote_amount,
			maker: maker_order.owner.clone(),
			taker: taker_order.owner.clone(),
			otype: taker_order.otype,
			price: maker_order.price,
		}
	}
}

///             LinkedItem          LinkedItem			LinkedItem          LinkedItem          LinkedItem
///             Bottom              Buy Order			Head                Sell Order          Top
///   			Next	    ---->   Price: 8	<----	Prev                Next       ---->    Price: max
///   max <---- Prev				Next		---->	Price:None  <----   Prev                Next        ---->   Price: 0
///         	Price:0		<----   Prev     			Next        ---->   Price 10   <----    Prev
///                                 Orders									Orders
///                                 o1: Hash -> buy 1@5						o101: Hash -> sell 100@10
///                                 o2: Hash -> buy 5@5						o102: Hash -> sell 100@5000
///                                 o3: Hash -> buy 100@5					
///                                 o4: Hash -> buy 40@5
///                                 o5: Hash -> buy 1000@5
#[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
pub struct LinkedItem<K1, K2> {
	pub price: Option<K2>,
	pub next: Option<K2>,
	pub prev: Option<K2>,
	pub orders: Vec<K1>,
}

pub struct LinkedList<T, S, K1, K2>(rstd::marker::PhantomData<(T, S, K1, K2)>);

// (TradePairHash, Price) => LinkedItem
impl<T, S, K1, K2> LinkedList<T, S, K1, K2> where
	T: Trait,
	K1: Parameter + Member + Copy + Clone + rstd::borrow::Borrow<<T as system::Trait>::Hash>,
	K2: Parameter + Member + Default + Bounded + SimpleArithmetic + Copy,
	S: StorageMap<(K1, Option<K2>), LinkedItem<K1, K2>, Query = Option<LinkedItem<K1, K2>>>,
{
	pub fn read_head(key: K1) -> LinkedItem<K1, K2> {
		Self::read(key, None)
	}

	pub fn read_bottom(key: K1) -> LinkedItem<K1, K2> {
		Self::read(key, Some(K2::min_value()))
	}

	pub fn read_top(key: K1) -> LinkedItem<K1, K2> {
		Self::read(key, Some(K2::max_value()))
	}

	pub fn read(key1: K1, key2: Option<K2>) -> LinkedItem<K1, K2> {
		S::get((key1, key2)).unwrap_or_else(|| {
			let bottom = LinkedItem {
				prev: Some(K2::max_value()),
				next: None,
				price: Some(K2::min_value()),
				orders: Vec::<K1>::new(),
			};
			
			let top = LinkedItem {
				prev: None,
				next: Some(K2::min_value()),
				price: Some(K2::max_value()),
				orders: Vec::<K1>::new(),
			};

			let item = LinkedItem {
				prev: Some(K2::min_value()),
				next: Some(K2::max_value()),
				price: None,
				orders: Vec::<K1>::new(),
			};
			Self::write(key1, key2, item.clone());
			Self::write(key1, bottom.price, bottom);
			Self::write(key1, top.price, top);
			
			item
		})
	}

	pub fn write(key1: K1, key2: Option<K2>, item: LinkedItem<K1, K2>) {
		S::insert((key1, key2), item);
	}

	pub fn append(key1: K1, key2: K2, order_hash: K1, otype: OrderType) {
		let item = S::get((key1, Some(key2)));
		match item {
			Some(mut item) => {
				item.orders.push(order_hash);
				Self::write(key1, Some(key2), item);
				return
			},
			None => {
				let start_item;
				let end_item;

				match otype {
					OrderType::Buy => {
						start_item = Some(K2::min_value());
						end_item = None;
					},
					OrderType::Sell => {
						start_item = None;
						end_item = Some(K2::max_value());
					},
				}

				let mut item = Self::read(key1, start_item);
				while item.next != end_item {
					match item.next {
						None => {},
						Some(price) => {
							if key2 < price {
								break;
							}
						}
					}

					item = Self::read(key1, item.next);
				}

				// add key2 after item

				// update new_prev
				let new_prev = LinkedItem {
					next: Some(key2),
					..item
				};
				Self::write(key1, new_prev.price, new_prev.clone());

				// update new next
				let next = Self::read(key1, item.next);
				let new_next = LinkedItem {
					prev: Some(key2),
					..next
				};
				Self::write(key1, new_next.price, new_next.clone());

				// insert new item
				let mut v = Vec::new();
				v.push(order_hash);
				let item = LinkedItem {
					prev: new_prev.price,
					next: new_next.price,
					price: Some(key2),
					orders: v,
				};
				Self::write(key1, Some(key2), item);
			},
		}
	}

	pub fn remove_items(key1: K1, otype: OrderType) -> Result {
		let end_item;

		if otype == OrderType::Buy {
			end_item = Some(K2::min_value());
		} else {
			end_item = Some(K2::max_value());
		}

		let mut head = Self::read_head(key1);

		loop {
			let key2;
			if otype == OrderType::Buy {
				key2 = head.prev;
			} else {
				key2 = head.next;
			}

			if key2 == end_item {
				break;
			}

			Self::remove_item(key1, key2.unwrap())?;
			head = Self::read_head(key1);
		}

		Ok(())
	}

	pub fn remove_item(key1: K1, key2: K2) -> Result {

		match S::get((key1, Some(key2))) {
			Some(mut item) => {
				while item.orders.len() > 0 {
					let order_hash = item.orders.get(0).ok_or("can not get order hash")?;

					let order = <Orders<T>>::get(order_hash.borrow()).ok_or("can not get order")?;

					ensure!(order.is_finished(), "try to remove not finished order");

					item.orders.remove(0);

					Self::write(key1, Some(key2), item.clone());
				}

				if item.orders.len() == 0 {
					if let Some(item) = S::take((key1, Some(key2))) {
						S::mutate((key1.clone(), item.prev), |x| {
							if let Some(x) = x {
								x.next = item.next;
							}
						});

						S::mutate((key1.clone(), item.next), |x| {
							if let Some(x) = x {
								x.prev = item.prev;
							}
						});
					}
				}
			},
			None => {},
		}

		Ok(())
	}
}

type OrderLinkedItem<T> = LinkedItem<<T as system::Trait>::Hash, <T as Trait>::Price>;
type OrderLinkedItemList<T> = LinkedList<T, LinkedItemList<T>, <T as system::Trait>::Hash, <T as Trait>::Price>;

decl_storage! {
	trait Store for Module<T: Trait> as trade {
		// TradePairHash => TradePair
		TradePairsByHash get(trade_pair_by_hash): map T::Hash => Option<TradePair<T::Hash>>;

		// (BaseTokenHash, QuoteTokenHash) => TradePairHash
		TradePairHashByBaseQuote get(get_trade_pair_hash_by_base_quote): map (T::Hash, T::Hash) => Option<T::Hash>;

		// OrderHash => Order
		Orders get(order): map T::Hash => Option<LimitOrder<T>>;

		// (AccountId, Index) => OrderHash
		OwnedOrders get(owned_order): map (T::AccountId, u64) => Option<T::Hash>;
		// AccountId => Index
		OwnedOrdersIndex get(owned_orders_index): map T::AccountId => u64;

		// (TradePairHash, Index) => OrderHash
		TradePairOwnedOrders get(trade_pair_owned_order): map (T::Hash, u64) => Option<T::Hash>;
		// TradePairHash => Index
		TradePairOwnedOrdersIndex get(trade_pair_owned_orders_index): map T::Hash => u64;

		// (TradePairHash, Price) => LinkedItem
		LinkedItemList get(sell_order): map (T::Hash, Option<T::Price>) => Option<OrderLinkedItem<T>>;

		// TradeHash => Trade
		Trades get(trade): map T::Hash => Option<Trade<T>>;

		// AccountId => Vec<TradeHash>
		OwnedTrades get(owned_trade): map T::AccountId => Option<Vec<T::Hash>>;
		// (AccountId, TradePairHash) => Vec<TradeHash>
		OwnedTPTrades get(owned_trade_pair_trade): map (T::AccountId, T::Hash) => Option<Vec<T::Hash>>;

		// TradePairHash => Vec<TradeHash>
		TradePairOwnedTrades get(trade_pair_owned_trade): map T::Hash => Option<Vec<T::Hash>>;

		// OrderHash => Vec<TradeHash>
		OrderOwnedTrades get(order_owned_trade): map T::Hash => Option<Vec<T::Hash>>;

		Nonce: u64;
	}
}

impl<T> OrderOwnedTrades<T> where T: Trait {
	fn add_trade(order_hash: T::Hash, trade_hash: T::Hash) {
		let mut trades;
		if let Some(ts) = Self::get(order_hash) {
			trades = ts;
		} else {
			trades = Vec::new();
		}

		trades.push(trade_hash);
		Self::insert(order_hash, trades);
	}
}

impl<T> TradePairOwnedTrades<T> where T: Trait {
	fn add_trade(tp_hash: T::Hash, trade_hash: T::Hash) {
		let mut trades;
		if let Some(ts) = Self::get(tp_hash) {
			trades = ts;
		} else {
			trades = Vec::new();
		}

		trades.push(trade_hash);
		Self::insert(tp_hash, trades);
	}
}

impl<T> OwnedTrades<T> where T: Trait {
	fn add_trade(account_id: T::AccountId, trade_hash: T::Hash) {
		let mut trades;
		if let Some(ts) = Self::get(account_id.clone()) {
			trades = ts;
		} else {
			trades = Vec::new();
		}

		trades.push(trade_hash);
		Self::insert(account_id, trades);
	}
}

const price_factor: u64 = 100_000_000;

impl<T> OwnedTPTrades<T> where T: Trait {
	fn add_trade(account_id: T::AccountId, tp_hash: T::Hash, trade_hash: T::Hash) {
		let mut trades;
		if let Some(ts) = Self::get((account_id.clone(), tp_hash)) {
			trades = ts;
		} else {
			trades = Vec::new();
		}

		trades.push(trade_hash);
		Self::insert((account_id, tp_hash), trades);
	}
}

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		fn deposit_event<T>() = default;

		pub fn create_trade_pair(origin, base: T::Hash, quote: T::Hash) -> Result {
			Self::do_create_trade_pair(origin, base, quote)
		}

		pub fn create_limit_order(origin, base: T::Hash, quote: T::Hash, otype: OrderType, price: T::Price, sell_amount: T::Balance) -> Result {
			let sender = ensure_signed(origin)?;

			let tp = Self::ensure_trade_pair(base, quote)?;

			Self::ensure_bonds(price, sell_amount)?;
			let buy_amount = Self::ensure_counterparty_amount_bonds(otype, price, sell_amount)?;

			let op_token_hash;
			match otype {
				OrderType::Buy => op_token_hash = base,
				OrderType::Sell => op_token_hash = quote,
			};

			let order = LimitOrder::<T>::new(base, quote, sender.clone(), price, sell_amount, buy_amount, otype);

			let hash = order.hash;

			<token::Module<T>>::ensure_free_balance(sender.clone(), op_token_hash, sell_amount)?;
			<token::Module<T>>::do_freeze(sender.clone(), op_token_hash, sell_amount)?;

			<Orders<T>>::insert(hash, order.clone());

			let owned_index = Self::owned_orders_index(sender.clone());
			<OwnedOrders<T>>::insert((sender.clone(), owned_index), hash);
			<OwnedOrdersIndex<T>>::insert(sender.clone(), owned_index + 1);

			let tp_owned_index = Self::trade_pair_owned_orders_index(tp);
			<TradePairOwnedOrders<T>>::insert((tp, tp_owned_index), hash);
			<TradePairOwnedOrdersIndex<T>>::insert(tp, tp_owned_index + 1);

			let filled: bool = Self::order_match(tp, order.clone())?;

			if !filled {
				<OrderLinkedItemList<T>>::append(tp, price, hash, otype);
			}

			Self::deposit_event(RawEvent::OrderCreated(sender, base, quote, hash, price, sell_amount));

			Ok(())
		}
	}
}

impl<T: Trait> Module<T> {
	fn ensure_bonds(price: T::Price, sell_amount: T::Balance) -> Result {
		ensure!(price > Zero::zero() && price <= T::Price::max_value(), "price bonds check failed");
		ensure!(sell_amount > Zero::zero() && sell_amount <= T::Balance::max_value(), "sell amount bonds check failed");

		Ok(())
	}

	fn ensure_counterparty_amount_bonds(otype: OrderType, price: T::Price, amount: T::Balance) -> result::Result<T::Balance, &'static str> {
		let price_u256 = U256::from(price.as_());
		let amount_u256 = U256::from(amount.as_());
		let price_factor_u256 = U256::from(price_factor);

		let amount_v2: U256;
		let counterparty_amount: U256;

		match otype {
			OrderType::Buy => {
				counterparty_amount = amount_u256 * price_factor_u256 / price_u256;
				amount_v2 = counterparty_amount * price_u256 / price_factor_u256;
			},
			OrderType::Sell => {
				counterparty_amount = amount_u256 * price_u256 / price_factor_u256;
				amount_v2 = counterparty_amount * price_factor_u256 / price_u256;
			},
		}

		if amount_v2 != amount_u256 {
			return Err("amount have digits parts")
		}

		if counterparty_amount == 0u32.into() || counterparty_amount > T::Balance::max_value().as_().into() {
			return Err("counterparty bound check failed")
		}

		let result: u64 = counterparty_amount.try_into().map_err(|_| "Overflow error")?;

		Ok(<T as balances::Trait>::Balance::sa(result))
	}

	fn next_match_price(item: &OrderLinkedItem<T>, otype: OrderType) -> Option<T::Price> {
		if otype == OrderType::Buy {
			item.prev
		} else {
			item.next
		}
	}

	fn price_matched(order_price: T::Price, order_type: OrderType, linked_item_price: T::Price) -> bool {
		match order_type {
			OrderType::Buy => order_price >= linked_item_price,
			OrderType::Sell => order_price <= linked_item_price,
		}
	}

	fn order_match(tp_hash: T::Hash, mut order: LimitOrder<T>) -> result::Result<bool, &'static str> {
		let mut head = <OrderLinkedItemList<T>>::read_head(tp_hash);

		let end_item_price;
		let otype = order.otype;
		let oprice = order.price;

		if otype == OrderType::Buy {
			end_item_price = Some(T::Price::min_value());
		} else {
			end_item_price = Some(T::Price::max_value());
		}
		
		let tp = Self::trade_pair_by_hash(tp_hash).ok_or("can not get trade pair")?;

		let give: T::Hash;
		let have: T::Hash;

		match otype {
			OrderType::Buy => {
				give = tp.base;
				have = tp.quote;
			},
			OrderType::Sell => {
				have = tp.base;
				give = tp.quote;
			},
		};

		loop {
			if order.status == OrderStatus::Filled {
				break;
			}

			let item_price = Self::next_match_price(&head, !otype);

			if item_price == end_item_price {
				break;
			}

			let item_price = item_price.ok_or("can not get item price")?;

			if !Self::price_matched(oprice, otype, item_price) {
				break;
			}

			let item = <LinkedItemList<T>>::get((tp_hash, Some(item_price))).ok_or("can not unwrap linked list item")?;

			for o in item.orders.iter() {
				let mut o = Self::order(o).ok_or("can not get order")?;

				// let ex_amount = order.remained_sell_amount.min(o.remained_sell_amount);

				let (base_qty, quote_qty) = Self::calculate_ex_amount(&o, &order)?;

				let give_qty: T::Balance;
				let have_qty: T::Balance;

				match otype {
					OrderType::Buy => {
						give_qty = base_qty;
						have_qty = quote_qty;
					},
					OrderType::Sell => {
						give_qty = quote_qty;
						have_qty = base_qty;
					},
				}

				<token::Module<T>>::do_unfreeze(order.owner.clone(), give, give_qty)?;
				<token::Module<T>>::do_unfreeze(o.owner.clone(), have, have_qty)?;

				<token::Module<T>>::do_transfer(order.owner.clone(), o.owner.clone(), give, give_qty)?;
				<token::Module<T>>::do_transfer(o.owner.clone(), order.owner.clone(), have, have_qty)?;

				if order.status == OrderStatus::Created {
					order.status = OrderStatus::PartialFilled;
				}

				if o.status == OrderStatus::Created {
					o.status = OrderStatus::PartialFilled;
				}

				order.remained_sell_amount = order.remained_sell_amount.checked_sub(&give_qty).ok_or("substract error")?;
				o.remained_sell_amount = o.remained_sell_amount.checked_sub(&have_qty).ok_or("substract error")?;

				order.remained_buy_amount = order.remained_buy_amount.checked_sub(&have_qty).ok_or("substract error")?;
				o.remained_buy_amount = o.remained_buy_amount.checked_sub(&give_qty).ok_or("substract error")?;

				if order.remained_buy_amount == Zero::zero() {
					order.status = OrderStatus::Filled;
					if order.remained_sell_amount != Zero::zero() {
						<token::Module<T>>::do_unfreeze(order.owner.clone(), give, order.remained_sell_amount)?;
						order.remained_sell_amount = Zero::zero();
					}
				}

				if o.remained_buy_amount == Zero::zero() {
					o.status = OrderStatus::Filled;
					if o.remained_sell_amount != Zero::zero() {
						<token::Module<T>>::do_unfreeze(o.owner.clone(), have, o.remained_sell_amount)?;
						o.remained_sell_amount = Zero::zero();
					}
				}

				<Orders<T>>::insert(order.hash, order.clone());
				<Orders<T>>::insert(o.hash, o.clone());

				let trade = Trade::new(tp.base, tp.quote, &o, &order, base_qty, quote_qty);
				<Trades<T>>::insert(trade.hash, trade.clone());
				
				// order owned trades, 2
				<OrderOwnedTrades<T>>::add_trade(order.hash, trade.hash);
				<OrderOwnedTrades<T>>::add_trade(o.hash, trade.hash);

				// acount owned trades, 2
				<OwnedTrades<T>>::add_trade(order.owner.clone(), trade.hash);
				<OwnedTrades<T>>::add_trade(o.owner.clone(), trade.hash);

				// acount + tp_hash woned trades, 2
				<OwnedTPTrades<T>>::add_trade(order.owner.clone(), tp_hash, trade.hash);
				<OwnedTPTrades<T>>::add_trade(o.owner.clone(), tp_hash, trade.hash);

				// trade pair owned trades, 1
				<TradePairOwnedTrades<T>>::add_trade(tp_hash, trade.hash);

				if order.status == OrderStatus::Filled {
					break
				}
			}

			head = <OrderLinkedItemList<T>>::read(tp_hash, Some(item_price));
		}

		<OrderLinkedItemList<T>>::remove_items(tp_hash, !otype);

		if order.status == OrderStatus::Filled {
			Ok(true)
		} else {
			Ok(false)
		}
	}

	pub fn calculate_ex_amount(maker_order: &LimitOrder<T>, taker_order: &LimitOrder<T>) -> result::Result<(T::Balance, T::Balance), &'static str> {
		let buyer_order;
		let seller_order;

		if taker_order.otype == OrderType::Buy {
			buyer_order = taker_order;
			seller_order = maker_order;
		} else {
			buyer_order = maker_order;
			seller_order = taker_order;
		}

		if seller_order.remained_buy_amount <= buyer_order.remained_sell_amount {
			let mut quote_qty: u64 = seller_order.remained_buy_amount.as_() * price_factor / maker_order.price.into();
			let buy_amount_v2 = quote_qty * maker_order.price.into() / price_factor;
			if buy_amount_v2 != seller_order.remained_buy_amount.as_() { // seller need give more to align
				quote_qty = quote_qty + 1;
			}

			return Ok((seller_order.remained_buy_amount, <T::Balance as As<u64>>::sa(quote_qty)))

		} else if buyer_order.remained_buy_amount <= seller_order.remained_sell_amount {

			let mut base_qty: u64 = buyer_order.remained_buy_amount.as_() * maker_order.price.into() /  price_factor;
			let buy_amount_v2 = base_qty * price_factor / maker_order.price.into();
			if buy_amount_v2 != buyer_order.remained_buy_amount.as_() { // buyer need give more to align
				base_qty = base_qty + 1;
			}

			return Ok((<T::Balance as As<u64>>::sa(base_qty), buyer_order.remained_buy_amount))
		}

		return Err("should never executed here")
	}

	pub fn ensure_trade_pair(base: T::Hash, quote: T::Hash) -> result::Result<T::Hash, &'static str> {
		let tp = Self::get_trade_pair_hash_by_base_quote((base, quote));
		ensure!(tp.is_some(), "");

		match tp {
			Some(tp) => Ok(tp),
			None => Err(""),
		}
	}

	pub fn do_create_trade_pair(origin: T::Origin, base: T::Hash, quote: T::Hash) -> Result {
		let sender = ensure_signed(origin)?;

		ensure!(base != quote, "base can not equal to quote");

		let base_owner = <token::Module<T>>::owner(base);
		let quote_owner = <token::Module<T>>::owner(quote);

		ensure!(base_owner.is_some() && quote_owner.is_some(), "");

		let base_owner = base_owner.unwrap();
		let quote_owner = quote_owner.unwrap();

		ensure!(sender == base_owner || sender == quote_owner, "");

		let bq = Self::get_trade_pair_hash_by_base_quote((base, quote));
		let qb = Self::get_trade_pair_hash_by_base_quote((quote, base));

		ensure!(!bq.is_some() && !qb.is_some(), "");

		let nonce = <Nonce<T>>::get();

		let hash = (base, quote, nonce, sender.clone(), <system::Module<T>>::random_seed()).using_encoded(<T as system::Trait>::Hashing::hash);

		let tp = TradePair {
			hash, base, quote
		};

		<Nonce<T>>::mutate(|n| *n += 1);
		<TradePairsByHash<T>>::insert(hash, tp.clone());
		<TradePairHashByBaseQuote<T>>::insert((base, quote), hash);

		Self::deposit_event(RawEvent::TradePairCreated(sender, hash, base, quote, tp));

		Ok(())
	}
}

decl_event!(
	pub enum Event<T> 
	where 
		<T as system::Trait>::AccountId,
		<T as system::Trait>::Hash,
		<T as Trait>::Price,
		<T as balances::Trait>::Balance,
		TradePair = TradePair<<T as system::Trait>::Hash>,
	{
		TradePairCreated(AccountId, Hash, Hash, Hash, TradePair),
		OrderCreated(AccountId, Hash, Hash, Hash, Price, Balance),
	}
);

/// tests for this module
#[cfg(test)]
mod tests {
	use super::*;

	use runtime_io::with_externalities;
	use primitives::{H256, Blake2Hasher};
	use support::{impl_outer_origin, assert_ok, assert_err};
	use runtime_primitives::{
		BuildStorage,
		traits::{BlakeTwo256, IdentityLookup},
		testing::{Digest, DigestItem, Header}
	};

	impl_outer_origin! {
		pub enum Origin for Test {}
	}

	// For testing the module, we construct most of a mock runtime. This means
	// first constructing a configuration type (`Test`) which `impl`s each of the
	// configuration traits of modules we want to use.
	#[derive(Clone, Eq, PartialEq, Debug)]
	pub struct Test;
	impl system::Trait for Test {
		type Origin = Origin;
		type Index = u64;
		type BlockNumber = u64;
		type Hash = H256;
		type Hashing = BlakeTwo256;
		type Digest = Digest;
		type AccountId = u64;
		type Lookup = IdentityLookup<Self::AccountId>;
		type Header = Header;
		type Event = ();
		type Log = DigestItem;
	}

	impl balances::Trait for Test {
		type Balance = u128;

		type OnFreeBalanceZero = ();

		type OnNewAccount = ();

		type Event = ();

		type TransactionPayment = ();
		type DustRemoval = ();
		type TransferPayment = ();
	}

	impl token::Trait for Test {
		type Event = ();
	}

	impl super::Trait for Test {
		type Event = ();
		type Price = u64;
	}

	type TokenModule = token::Module<Test>;
	type TradeModule = super::Module<Test>;

	// This function basically just builds a genesis storage key/value store according to
	// our desired mockup.
	fn new_test_ext() -> runtime_io::TestExternalities<Blake2Hasher> {
		system::GenesisConfig::<Test>::default().build_storage().unwrap().0.into()
	}

	fn output_order(tp_hash: <Test as system::Trait>::Hash) {

		let mut item = <OrderLinkedItemList<Test>>::read_bottom(tp_hash);

		println!("[Market Orders]");

		loop {
			if item.price == Some(<Test as Trait>::Price::min_value()) {
				print!("Bottom ==> ");
			} else if item.price == Some(<Test as Trait>::Price::max_value()) {
				print!("Top ==> ");
			} else if item.price == None {
				print!("Head ==> ");
			}

			print!("Price({:?}), Next({:?}), Prev({:?}), Orders({}): ", item.price, item.next, item.prev, item.orders.len());

			let mut orders = item.orders.iter();
			loop {
				match orders.next() {
					Some(order_hash) => {
						let order = <Orders<Test>>::get(order_hash).unwrap();
						print!("({}@[{:?}]: {}, {}), ", order.hash, order.status, order.amount, order.remained_sell_amount);
					},
					None => break,
				}
			}

			println!("");

			if item.next == Some(<Test as Trait>::Price::min_value()) {
				break;
			} else {
				item = OrderLinkedItemList::<Test>::read(tp_hash, item.next);
			}
		}

		println!("[Market Trades]");

		let trades = TradeModule::trade_pair_owned_trade(tp_hash);
		if let Some(trades) = trades {
			for hash in trades.iter() {
				let trade = <Trades<Test>>::get(hash).unwrap();
				println!("[{}/{}] - {}@{}[{:?}]: [Buyer,Seller][{},{}], [Maker,Taker][{},{}], [Base,Quote][{}, {}]", 
					trade.quote, trade.base, hash, trade.price, trade.otype, trade.buyer, trade.seller, trade.maker, 
					trade.taker, trade.base_amount, trade.quote_amount);
			}
		}

		println!();
	}

	#[test]
	fn linked_list_test_case() {
		with_externalities(&mut new_test_ext(), || {
			let ALICE = 10;
			let BOB = 20;
			let CHARLIE = 30;

			let max = Some(<Test as Trait>::Price::max_value());
			let min = Some(<Test as Trait>::Price::min_value());

			// token1
			assert_ok!(TokenModule::issue(Origin::signed(ALICE), b"66".to_vec(), 21000000));
			let token1_hash = TokenModule::owned_token((ALICE, 0)).unwrap();
			let token1 = TokenModule::token(token1_hash).unwrap();

			// token2
			assert_ok!(TokenModule::issue(Origin::signed(BOB), b"77".to_vec(), 10000000));
			let token2_hash = TokenModule::owned_token((BOB, 0)).unwrap();
			let token2 = TokenModule::token(token2_hash).unwrap();

			// tradepair
			let base = token1.hash;
			let quote = token2.hash;
			assert_ok!(TradeModule::create_trade_pair(Origin::signed(ALICE), base, quote));
			let tp_hash = TradeModule::get_trade_pair_hash_by_base_quote((base, quote)).unwrap();
			let tp = TradeModule::trade_pair_by_hash(tp_hash).unwrap();

			let bottom = OrderLinkedItem::<Test> {
				prev: max,
				next: None,
				price: min,
				orders: Vec::new(),
			};

			let top = OrderLinkedItem::<Test> {
				prev: None,
				next: min,
				price: max,
				orders: Vec::new(),
			};

			let head = OrderLinkedItem::<Test> {
				prev: min,
				next: max,
				price: None,
				orders: Vec::new(),
			};

			assert_eq!(head, <OrderLinkedItemList<Test>>::read_head(tp_hash));
			assert_eq!(bottom, <OrderLinkedItemList<Test>>::read_bottom(tp_hash));
			assert_eq!(top, <OrderLinkedItemList<Test>>::read_top(tp_hash));

			output_order(tp_hash);

			// sell limit order
			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 18, 100));
			let order1_hash = TradeModule::owned_order((BOB, 0)).unwrap();
			let mut order1 = TradeModule::order(order1_hash).unwrap();
			assert_eq!(order1.amount, 100);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 10, 50));
			let order2_hash = TradeModule::owned_order((BOB, 1)).unwrap();
			let mut order2 = TradeModule::order(order2_hash).unwrap();
			assert_eq!(order2.amount, 50);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 5, 10));
			let order3_hash = TradeModule::owned_order((BOB, 2)).unwrap();
			let mut order3 = TradeModule::order(order3_hash).unwrap();
			assert_eq!(order3.amount, 10);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 5, 20));
			let order4_hash = TradeModule::owned_order((BOB, 3)).unwrap();
			let mut order4 = TradeModule::order(order4_hash).unwrap();
			assert_eq!(order4.amount, 20);
			
			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 12, 10));
			let order5_hash = TradeModule::owned_order((BOB, 4)).unwrap();
			let mut order5 = TradeModule::order(order5_hash).unwrap();
			assert_eq!(order5.amount, 10);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 12, 30));
			let order6_hash = TradeModule::owned_order((BOB, 5)).unwrap();
			let mut order6 = TradeModule::order(order6_hash).unwrap();
			assert_eq!(order6.amount, 30);
			
			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 12, 20));
			let order7_hash = TradeModule::owned_order((BOB, 6)).unwrap();
			let mut order7 = TradeModule::order(order7_hash).unwrap();
			assert_eq!(order7.amount, 20);

			// buy limit order
			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 2, 5));
			let order101_hash = TradeModule::owned_order((ALICE, 0)).unwrap();
			let mut order101 = TradeModule::order(order101_hash).unwrap();
			assert_eq!(order101.amount, 5);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 1, 12));
			let order102_hash = TradeModule::owned_order((ALICE, 1)).unwrap();
			let mut order102 = TradeModule::order(order102_hash).unwrap();
			assert_eq!(order102.amount, 12);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 4, 100));
			let order103_hash = TradeModule::owned_order((ALICE, 2)).unwrap();
			let mut order103 = TradeModule::order(order103_hash).unwrap();
			assert_eq!(order103.amount, 100);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 2, 1000000));
			let order104_hash = TradeModule::owned_order((ALICE, 3)).unwrap();
			let mut order104 = TradeModule::order(order104_hash).unwrap();
			assert_eq!(order104.amount, 1000000);

			// head
			let mut item = OrderLinkedItem::<Test> {
				next: Some(5),
				prev: Some(4),
				price: None,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_head(tp_hash), item);

			// item1
			let mut curr = item.next;

			let mut v = Vec::new();
			v.push(order3_hash);
			v.push(order4_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(10),
				prev: None,
				price: Some(5),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();
			v.push(order2_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(12),
				prev: Some(5),
				price: Some(10),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item3
			curr = item.next;
			
			v = Vec::new();
			v.push(order5_hash);
			v.push(order6_hash);
			v.push(order7_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(18),
				prev: Some(10),
				price: Some(12),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item4
			curr = item.next;

			v = Vec::new();
			v.push(order1_hash);

			item = OrderLinkedItem::<Test> {
				next: max,
				prev: Some(12),
				price: Some(18),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// top
			item = OrderLinkedItem::<Test> {
				next: min,
				prev: Some(18),
				price: max,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_top(tp_hash), item);

			// bottom
			item = OrderLinkedItem::<Test> {
				next: Some(1),
				prev: max,
				price: min,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_bottom(tp_hash), item);

			// item1
			let mut curr = item.next;

			let mut v = Vec::new();
			v.push(order102_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(2),
				prev: min,
				price: Some(1),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();
			v.push(order101_hash);
			v.push(order104_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(4),
				prev: Some(1),
				price: Some(2),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item3
			curr = item.next;
			
			v = Vec::new();
			v.push(order103_hash);

			item = OrderLinkedItem::<Test> {
				next: None,
				prev: Some(2),
				price: Some(4),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// remove sell orders
			OrderLinkedItemList::<Test>::remove_items(tp_hash, OrderType::Sell);
			OrderLinkedItemList::<Test>::remove_items(tp_hash, OrderType::Buy);

			// Bottom ==> Price(Some(0)), Next(Some(1)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(1)), Next(Some(2)), Prev(Some(0)), Orders(1): (0x2063…669c : 12, 12), 
			// Price(Some(2)), Next(Some(4)), Prev(Some(1)), Orders(2): (0x5fe7…c31c : 5, 5), (0xb0a8…fb1a : 1000000, 1000000), 
			// Price(Some(4)), Next(None), Prev(Some(2)), Orders(1): (0x4293…b948 : 100, 100), 
			// Head ==> Price(None), Next(Some(5)), Prev(Some(4)), Orders(0): 
			// Price(Some(5)), Next(Some(10)), Prev(None), Orders(2): (0x6de6…98b4 : 10, 10), (0x895b…0377 : 20, 20), 
			// Price(Some(10)), Next(Some(12)), Prev(Some(5)), Orders(1): (0xc10f…32e3 : 50, 50), 
			// Price(Some(12)), Next(Some(18)), Prev(Some(10)), Orders(3): (0xefbf…d851 : 10, 10), (0xe71e…8be1 : 30, 30), (0xbbe2…36b9 : 20, 20), 
			// Price(Some(18)), Next(Some(18446744073709551615)), Prev(Some(12)), Orders(1): (0x8439…5abc : 100, 100), 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(Some(18)), Orders(0): 
			output_order(tp_hash);

			// price = 5
			order3.remained_sell_amount = Zero::zero();
			order3.status = OrderStatus::Filled;
			<Orders<Test>>::insert(order3.hash, order3);

			order4.status = OrderStatus::Canceled;
			<Orders<Test>>::insert(order4.hash, order4);

			// price = 10
			order2.remained_sell_amount = Zero::zero();
			order2.status = OrderStatus::Filled;
			<Orders<Test>>::insert(order2.hash, order2);

			// price = 12
			order5.status = OrderStatus::Canceled;
			<Orders<Test>>::insert(order5.hash, order5);

			order6.remained_sell_amount = order6.remained_sell_amount.checked_sub(1).unwrap();
			order6.status = OrderStatus::PartialFilled;
			<Orders<Test>>::insert(order6.hash, order6.clone());

			OrderLinkedItemList::<Test>::remove_items(tp_hash, OrderType::Sell);

			// head
			item = OrderLinkedItem::<Test> {
				next: Some(12),
				prev: Some(4),
				price: None,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_head(tp_hash), item);

			// item1
			curr = item.next;
			
			v = Vec::new();
			v.push(order6_hash);
			v.push(order7_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(18),
				prev: None,
				price: Some(12),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();
			v.push(order1_hash);

			item = OrderLinkedItem::<Test> {
				next: max,
				prev: Some(12),
				price: Some(18),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			<OrderLinkedItemList<Test>>::remove_items(tp_hash, OrderType::Sell);

			// Bottom ==> Price(Some(0)), Next(Some(1)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(1)), Next(Some(2)), Prev(Some(0)), Orders(1): (0x2063…669c : 12, 12), 
			// Price(Some(2)), Next(Some(4)), Prev(Some(1)), Orders(2): (0x5fe7…c31c : 5, 5), (0xb0a8…fb1a : 1000000, 1000000), 
			// Price(Some(4)), Next(None), Prev(Some(2)), Orders(1): (0x4293…b948 : 100, 100), 
			// Head ==> Price(None), Next(Some(12)), Prev(Some(4)), Orders(0): 
			// Price(Some(12)), Next(Some(18)), Prev(None), Orders(2): (0xe71e…8be1 : 30, 29), (0xbbe2…36b9 : 20, 20), 
			// Price(Some(18)), Next(Some(18446744073709551615)), Prev(Some(12)), Orders(1): (0x8439…5abc : 100, 100), 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(Some(18)), Orders(0): 
			output_order(tp_hash);

			// price = 18
			order1.status = OrderStatus::Canceled;
			<Orders<Test>>::insert(order1.hash, order1);

			// price = 12
			order6.remained_sell_amount = Zero::zero();
			order6.status = OrderStatus::Filled;
			<Orders<Test>>::insert(order6.hash, order6);

			order7.remained_sell_amount = Zero::zero();
			order7.status = OrderStatus::Filled;
			<Orders<Test>>::insert(order7.hash, order7);

			<OrderLinkedItemList<Test>>::remove_items(tp_hash, OrderType::Sell);

			// head
			item = OrderLinkedItem::<Test> {
				next: max,
				prev: Some(4),
				price: None,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_head(tp_hash), item);

			// Bottom ==> Price(Some(0)), Next(Some(1)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(1)), Next(Some(2)), Prev(Some(0)), Orders(1): (0x2063…669c : 12, 12), 
			// Price(Some(2)), Next(Some(4)), Prev(Some(1)), Orders(2): (0x5fe7…c31c : 5, 5), (0xb0a8…fb1a : 1000000, 1000000), 
			// Price(Some(4)), Next(None), Prev(Some(2)), Orders(1): (0x4293…b948 : 100, 100), 
			// Head ==> Price(None), Next(Some(18446744073709551615)), Prev(Some(4)), Orders(0): 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(None), Orders(0): 
			output_order(tp_hash);

			// remove buy orders
			// price = 4
			order103.remained_sell_amount = Zero::zero();
			order103.status = OrderStatus::Filled;
			<Orders<Test>>::insert(order103.hash, order103);

			// price = 2
			order101.status = OrderStatus::Canceled;
			<Orders<Test>>::insert(order101.hash, order101);

			order104.remained_sell_amount = order104.remained_sell_amount.checked_sub(100).unwrap();
			order104.status = OrderStatus::PartialFilled;
			<Orders<Test>>::insert(order104.hash, order104.clone());

			<OrderLinkedItemList<Test>>::remove_items(tp_hash, OrderType::Buy);

			// bottom
			item = OrderLinkedItem::<Test> {
				next: Some(1),
				prev: max,
				price: min,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_bottom(tp_hash), item);

			// item1
			let mut curr = item.next;

			let mut v = Vec::new();
			v.push(order102_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(2),
				prev: min,
				price: Some(1),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();
			v.push(order104_hash);

			item = OrderLinkedItem::<Test> {
				next: None,
				prev: Some(1),
				price: Some(2),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// Bottom ==> Price(Some(0)), Next(Some(1)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(1)), Next(Some(2)), Prev(Some(0)), Orders(1): (0x2063…669c : 12, 12), 
			// Price(Some(2)), Next(None), Prev(Some(1)), Orders(1): (0xb0a8…fb1a : 1000000, 999900), 
			// Head ==> Price(None), Next(Some(18446744073709551615)), Prev(Some(2)), Orders(0): 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(None), Orders(0):
			output_order(tp_hash);

			// price = 2
			order104.status = OrderStatus::Canceled;
			<Orders<Test>>::insert(order104.hash, order104);

			// price = 1
			order102.remained_sell_amount = Zero::zero();
			order102.status = OrderStatus::Filled;
			<Orders<Test>>::insert(order102.hash, order102);

			<OrderLinkedItemList<Test>>::remove_items(tp_hash, OrderType::Buy);

			let bottom = OrderLinkedItem::<Test> {
				prev: max,
				next: None,
				price: min,
				orders: Vec::new(),
			};

			let top = OrderLinkedItem::<Test> {
				prev: None,
				next: min,
				price: max,
				orders: Vec::new(),
			};

			let head = OrderLinkedItem::<Test> {
				prev: min,
				next: max,
				price: None,
				orders: Vec::new(),
			};

			assert_eq!(head, <OrderLinkedItemList<Test>>::read_head(tp_hash));
			assert_eq!(bottom, <OrderLinkedItemList<Test>>::read_bottom(tp_hash));
			assert_eq!(top, <OrderLinkedItemList<Test>>::read_top(tp_hash));			

			// Bottom ==> Price(Some(0)), Next(None), Prev(Some(18446744073709551615)), Orders(0): 
			// Head ==> Price(None), Next(Some(18446744073709551615)), Prev(Some(0)), Orders(0): 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(None), Orders(0):
			output_order(tp_hash);
		});
	}

	#[test]
	fn order_match_test_case() {
		with_externalities(&mut new_test_ext(), || {
			let ALICE = 10;
			let BOB = 20;
			let CHARLIE = 30;

			let max = Some(<Test as Trait>::Price::max_value());
			let min = Some(<Test as Trait>::Price::min_value());

			// token1
			assert_ok!(TokenModule::issue(Origin::signed(ALICE), b"66".to_vec(), 21000000));
			let token1_hash = TokenModule::owned_token((ALICE, 0)).unwrap();
			let token1 = TokenModule::token(token1_hash).unwrap();

			// token2
			assert_ok!(TokenModule::issue(Origin::signed(BOB), b"77".to_vec(), 10000000));
			let token2_hash = TokenModule::owned_token((BOB, 0)).unwrap();
			let token2 = TokenModule::token(token2_hash).unwrap();

			// tradepair
			let base = token1.hash;
			let quote = token2.hash;
			assert_ok!(TradeModule::create_trade_pair(Origin::signed(ALICE), base, quote));
			let tp_hash = TradeModule::get_trade_pair_hash_by_base_quote((base, quote)).unwrap();
			let tp = TradeModule::trade_pair_by_hash(tp_hash).unwrap();

			let bottom = OrderLinkedItem::<Test> {
				prev: max,
				next: None,
				price: min,
				orders: Vec::new(),
			};

			let top = OrderLinkedItem::<Test> {
				prev: None,
				next: min,
				price: max,
				orders: Vec::new(),
			};

			let head = OrderLinkedItem::<Test> {
				prev: min,
				next: max,
				price: None,
				orders: Vec::new(),
			};

			assert_eq!(head, <OrderLinkedItemList<Test>>::read_head(tp_hash));
			assert_eq!(bottom, <OrderLinkedItemList<Test>>::read_bottom(tp_hash));
			assert_eq!(top, <OrderLinkedItemList<Test>>::read_top(tp_hash));	

			output_order(tp_hash);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 18, 200));
			let order1_hash = TradeModule::owned_order((BOB, 0)).unwrap();
			let mut order1 = TradeModule::order(order1_hash).unwrap();
			assert_eq!(order1.amount, 200);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 10, 11));
			let order2_hash = TradeModule::owned_order((BOB, 1)).unwrap();
			let mut order2 = TradeModule::order(order2_hash).unwrap();
			assert_eq!(order2.amount, 11);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 11, 10));
			let order3_hash = TradeModule::owned_order((BOB, 2)).unwrap();
			let mut order3 = TradeModule::order(order3_hash).unwrap();
			assert_eq!(order3.amount, 10);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(BOB), base, quote, OrderType::Sell, 11, 10000));
			let order4_hash = TradeModule::owned_order((BOB, 3)).unwrap();
			let mut order4 = TradeModule::order(order4_hash).unwrap();
			assert_eq!(order4.amount, 10000);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 6, 50));
			let order101_hash = TradeModule::owned_order((ALICE, 0)).unwrap();
			let mut order101 = TradeModule::order(order101_hash).unwrap();
			assert_eq!(order101.amount, 50);

			// bottom
			let mut item = OrderLinkedItem::<Test> {
				next: Some(6),
				prev: max,
				price: min,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_bottom(tp_hash), item);

			// item1
			let mut curr = item.next;

			let mut v = Vec::new();
			v.push(order101_hash);

			item = OrderLinkedItem::<Test> {
				next: None,
				prev: Some(0),
				price: Some(6),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();

			item = OrderLinkedItem::<Test> {
				next: Some(10),
				prev: Some(6),
				price: None,
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item3
			curr = item.next;
			
			v = Vec::new();
			v.push(order2_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(11),
				prev: None,
				price: Some(10),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item4
			curr = item.next;

			v = Vec::new();
			v.push(order3_hash);
			v.push(order4_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(18),
				prev: Some(10),
				price: Some(11),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item5
			curr = item.next;

			v = Vec::new();
			v.push(order1_hash);

			item = OrderLinkedItem::<Test> {
				next: max,
				prev: Some(11),
				price: Some(18),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// top
			item = OrderLinkedItem::<Test> {
				next: min,
				prev: Some(18),
				price: max,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_top(tp_hash), item);

			// Bottom ==> Price(Some(0)), Next(Some(6)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(6)), Next(None), Prev(Some(0)), Orders(1): (0x245e…2743@[Created]: 50, 50), 
			// Head ==> Price(None), Next(Some(10)), Prev(Some(6)), Orders(0): 
			// Price(Some(10)), Next(Some(11)), Prev(None), Orders(1): (0x8a65…d603@[Created]: 11, 11), 
			// Price(Some(11)), Next(Some(18)), Prev(Some(10)), Orders(2): (0x95a8…7a9d@[Created]: 10, 10), (0x89d7…6e94@[Created]: 10000, 10000), 
			// Price(Some(18)), Next(Some(18446744073709551615)), Prev(Some(11)), Orders(1): (0x1396…8c14@[Created]: 200, 200), 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(Some(18)), Orders(0): 
			output_order(tp_hash);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 11, 51));
			let order102_hash = TradeModule::owned_order((ALICE, 1)).unwrap();
			let mut order102 = TradeModule::order(order102_hash).unwrap();
			assert_eq!(order102.amount, 51);
			assert_eq!(order102.remained_sell_amount, 0);
			assert_eq!(order102.status, OrderStatus::Filled);

			order2 = TradeModule::order(order2_hash).unwrap();
			assert_eq!(order2.amount, 11);
			assert_eq!(order2.remained_sell_amount, 0);
			assert_eq!(order2.status, OrderStatus::Filled);

			order3 = TradeModule::order(order3_hash).unwrap();
			assert_eq!(order3.amount, 10);
			assert_eq!(order3.remained_sell_amount, 0);
			assert_eq!(order3.status, OrderStatus::Filled);

			order4 = TradeModule::order(order4_hash).unwrap();
			assert_eq!(order4.amount, 10000);
			assert_eq!(order4.remained_sell_amount, 9970);
			assert_eq!(order4.status, OrderStatus::PartialFilled);

			// bottom
			let mut item = OrderLinkedItem::<Test> {
				next: Some(6),
				prev: max,
				price: min,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_bottom(tp_hash), item);

			// item1
			let mut curr = item.next;

			let mut v = Vec::new();
			v.push(order101_hash);

			item = OrderLinkedItem::<Test> {
				next: None,
				prev: Some(0),
				price: Some(6),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();

			item = OrderLinkedItem::<Test> {
				next: Some(11),
				prev: Some(6),
				price: None,
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item4
			curr = item.next;

			v = Vec::new();
			v.push(order4_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(18),
				prev: None,
				price: Some(11),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item5
			curr = item.next;

			v = Vec::new();
			v.push(order1_hash);

			item = OrderLinkedItem::<Test> {
				next: max,
				prev: Some(11),
				price: Some(18),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// top
			item = OrderLinkedItem::<Test> {
				next: min,
				prev: Some(18),
				price: max,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_top(tp_hash), item);

			let trades = <TradePairOwnedTrades<Test>>::get(tp_hash).unwrap();
			let mut v = Vec::new();
			v.push(trades[0]);
			v.push(trades[1]);
			v.push(trades[2]);

			assert_eq!(<OwnedTrades<Test>>::get(ALICE), Some(v.clone()));
			assert_eq!(<OwnedTrades<Test>>::get(BOB), Some(v.clone()));

			assert_eq!(<OwnedTPTrades<Test>>::get((ALICE, tp_hash)), Some(v.clone()));
			assert_eq!(<OwnedTPTrades<Test>>::get((BOB, tp_hash)), Some(v.clone()));

			assert_eq!(<TradePairOwnedTrades<Test>>::get(tp_hash), Some(v.clone()));

			assert_eq!(<OrderOwnedTrades<Test>>::get(order102_hash).unwrap().len(), 3);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order102_hash), Some(v));

			let mut v = Vec::new();
			v.push(trades[0]);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order2_hash).unwrap().len(), 1);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order2_hash), Some(v));

			let mut v = Vec::new();
			v.push(trades[1]);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order3_hash).unwrap().len(), 1);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order3_hash), Some(v));

			let mut v = Vec::new();
			v.push(trades[2]);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order4_hash).unwrap().len(), 1);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order4_hash), Some(v));

			assert_eq!(trades.len(), 3);
			let t1 = <Trades<Test>>::get(trades[0]).unwrap();
			let trade1 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 10,
				base_amount: 11,
				quote_amount: 11,
				..t1
			};
			assert_eq!(t1, trade1);

			let t2 = <Trades<Test>>::get(trades[1]).unwrap();
			let trade2 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 11,
				base_amount: 10,
				quote_amount: 10,
				..t2
			};
			assert_eq!(t2, trade2);

			let t3 = <Trades<Test>>::get(trades[2]).unwrap();
			let trade3 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 11,
				base_amount: 30,
				quote_amount: 30,
				..t3
			};
			assert_eq!(t3, trade3);

			// [Market Orders]
			// Bottom ==> Price(Some(0)), Next(Some(6)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(6)), Next(None), Prev(Some(0)), Orders(1): (0x245e…2743@[Created]: 50, 50), 
			// Head ==> Price(None), Next(Some(11)), Prev(Some(6)), Orders(0): 
			// Price(Some(11)), Next(Some(18)), Prev(None), Orders(1): (0x89d7…6e94@[PartialFilled]: 10000, 9970), 
			// Price(Some(18)), Next(Some(18446744073709551615)), Prev(Some(11)), Orders(1): (0x1396…8c14@[Created]: 200, 200), 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(Some(18)), Orders(0): 
			// [Market Trades]
			// [0x72bb…80c0/0x8a33…f642] - 0xf163…386c@10[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][11, 11]
			// [0x72bb…80c0/0x8a33…f642] - 0xf9c3…edd3@11[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][10, 10]
			// [0x72bb…80c0/0x8a33…f642] - 0x975e…0c4d@11[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][30, 30]
			output_order(tp_hash);

			assert_ok!(TradeModule::create_limit_order(Origin::signed(ALICE), base, quote, OrderType::Buy, 18, 13211));
			let order103_hash = TradeModule::owned_order((ALICE, 2)).unwrap();
			let mut order103 = TradeModule::order(order103_hash).unwrap();
			assert_eq!(order103.amount, 13211);
			assert_eq!(order103.remained_sell_amount, 3041);
			assert_eq!(order103.status, OrderStatus::PartialFilled);

			order4 = TradeModule::order(order4_hash).unwrap();
			assert_eq!(order4.amount, 10000);
			assert_eq!(order4.remained_sell_amount, 0);
			assert_eq!(order4.status, OrderStatus::Filled);

			order1 = TradeModule::order(order1_hash).unwrap();
			assert_eq!(order1.amount, 200);
			assert_eq!(order1.remained_sell_amount, 0);
			assert_eq!(order1.status, OrderStatus::Filled);

			// bottom
			let mut item = OrderLinkedItem::<Test> {
				next: Some(6),
				prev: max,
				price: min,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_bottom(tp_hash), item);

			// item1
			let mut curr = item.next;

			let mut v = Vec::new();
			v.push(order101_hash);

			item = OrderLinkedItem::<Test> {
				next: Some(18),
				prev: Some(0),
				price: Some(6),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item2
			curr = item.next;

			v = Vec::new();
			v.push(order103_hash);

			item = OrderLinkedItem::<Test> {
				next: None,
				prev: Some(6),
				price: Some(18),
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// item4
			curr = item.next;

			v = Vec::new();

			item = OrderLinkedItem::<Test> {
				next: max,
				prev: Some(18),
				price: None,
				orders: v,
			};
			assert_eq!(OrderLinkedItemList::<Test>::read(tp_hash, curr), item);

			// top
			item = OrderLinkedItem::<Test> {
				next: min,
				prev: None,
				price: max,
				orders: Vec::new(),
			};
			assert_eq!(OrderLinkedItemList::<Test>::read_top(tp_hash), item);

			let trades = <TradePairOwnedTrades<Test>>::get(tp_hash).unwrap();
			let mut v = Vec::new();
			v.push(trades[0]);
			v.push(trades[1]);
			v.push(trades[2]);
			v.push(trades[3]);
			v.push(trades[4]);

			assert_eq!(<OwnedTrades<Test>>::get(ALICE), Some(v.clone()));
			assert_eq!(<OwnedTrades<Test>>::get(BOB), Some(v.clone()));

			assert_eq!(<OwnedTPTrades<Test>>::get((ALICE, tp_hash)), Some(v.clone()));
			assert_eq!(<OwnedTPTrades<Test>>::get((BOB, tp_hash)), Some(v.clone()));

			assert_eq!(<TradePairOwnedTrades<Test>>::get(tp_hash), Some(v.clone()));

			let mut v = Vec::new();
			v.push(trades[3]);
			v.push(trades[4]);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order103_hash).unwrap().len(), 2);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order103_hash), Some(v));

			let mut v = Vec::new();
			v.push(trades[2]);
			v.push(trades[3]);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order4_hash).unwrap().len(), 2);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order4_hash), Some(v));

			let mut v = Vec::new();
			v.push(trades[4]);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order1_hash).unwrap().len(), 1);
			assert_eq!(<OrderOwnedTrades<Test>>::get(order1_hash), Some(v));

			let trades = <TradePairOwnedTrades<Test>>::get(tp_hash).unwrap();
			assert_eq!(trades.len(), 5);
			let t1 = <Trades<Test>>::get(trades[0]).unwrap();
			let trade1 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 10,
				base_amount: 11,
				quote_amount: 11,
				..t1
			};
			assert_eq!(t1, trade1);

			let t2 = <Trades<Test>>::get(trades[1]).unwrap();
			let trade2 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 11,
				base_amount: 10,
				quote_amount: 10,
				..t2
			};
			assert_eq!(t2, trade2);

			let t3 = <Trades<Test>>::get(trades[2]).unwrap();
			let trade3 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 11,
				base_amount: 30,
				quote_amount: 30,
				..t3
			};
			assert_eq!(t3, trade3);

			let t4 = <Trades<Test>>::get(trades[3]).unwrap();
			let trade4 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 11,
				base_amount: 9970,
				quote_amount: 9970,
				..t4
			};
			assert_eq!(t4, trade4);

			let t5 = <Trades<Test>>::get(trades[4]).unwrap();
			let trade5 = Trade::<Test> {
				base: base,
				quote: quote,
				buyer: ALICE,
				seller: BOB,
				maker: BOB,
				taker: ALICE,
				otype: OrderType::Buy,
				price: 18,
				base_amount: 200,
				quote_amount: 200,
				..t5
			};
			assert_eq!(t5, trade5);

			// [Market Orders]
			// Bottom ==> Price(Some(0)), Next(Some(6)), Prev(Some(18446744073709551615)), Orders(0): 
			// Price(Some(6)), Next(Some(18)), Prev(Some(0)), Orders(1): (0x245e…2743@[Created]: 50, 50), 
			// Price(Some(18)), Next(None), Prev(Some(6)), Orders(1): (0xc98d…25d7@[PartialFilled]: 13211, 3041), 
			// Head ==> Price(None), Next(Some(18446744073709551615)), Prev(Some(18)), Orders(0): 
			// Top ==> Price(Some(18446744073709551615)), Next(Some(0)), Prev(None), Orders(0): 
			// [Market Trades]
			// [0x72bb…80c0/0x8a33…f642] - 0xf163…386c@10[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][11, 11]
			// [0x72bb…80c0/0x8a33…f642] - 0xf9c3…edd3@11[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][10, 10]
			// [0x72bb…80c0/0x8a33…f642] - 0x975e…0c4d@11[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][30, 30]
			// [0x72bb…80c0/0x8a33…f642] - 0x4957…4e49@11[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][9970, 9970]
			// [0x72bb…80c0/0x8a33…f642] - 0xa190…e0cb@18[Buy]: [Buyer,Seller][10,20], [Maker,Taker][20,10], [Base,Quote][200, 200]
			output_order(tp_hash);
		});
	}
}