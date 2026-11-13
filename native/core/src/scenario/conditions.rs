//! The operational conditions of a generated Scenario (Issue #298): `with_standard_provisioning`,
//! `OperationalConditions.compile`, `provenance_sources` and `referenced_source_ids` of
//! `region/conditions.py`, for the conditions the request contract lets through (`request.rs`):
//! the initial water and food, and the standard deliveries the provisioning adds. No equipment,
//! schedule, shift or infrastructure input reaches here, so only the initial stock and the
//! `resource_delivery` events are compiled.

use crate::scenario::profiles::{map, text, texts};
use crate::scenario::request::{InventoryStatus, OperationalConditions, DEFAULT_WAREHOUSE_ID};
use crate::value::V;

pub const RESOURCE_SERVICE_ASSUMPTION_ID: &str = "ASM-REGION-RESOURCE-SERVICE-V1";
pub const STANDARD_PROVISIONING_SOURCE_ID: &str = "ASM-STANDARD-PROVISIONING-V1";
pub const DRINKING_WATER_ML_PER_PERSON_DAY: i64 = 3_000;
pub const MEAL_UNITS_PER_PERSON_DAY: i64 = 3;
pub const STOCK_DAYS: i64 = 3;
const MAX_DEFAULT_DELIVERY_EVENTS: i64 = 64;
/// `MAX_RESOURCE_QUANTITY` of `experiments/scenario.py`.
pub const MAX_RESOURCE_QUANTITY: i64 = 1_000_000_000_000_000;

/// `default_resource_service_config().model_dump(mode="json")`, every field with its default.
/// The coefficients are the model's (a teammate's); `native/core/tests/scenario_conditions.rs`
/// checks this text against what Python dumps today (`data/scenario_conditions.json`).
pub const RESOURCE_SERVICE_CONFIG_JSON: &str = r#"{"schema_version":1,"policy_version":"resource-service-v1","source_id":"ASM-REGION-RESOURCE-SERVICE-V1","source_type":"assumption","service_minutes":5,"water_ml_per_service":250,"food_meal_units_per_service":1,"hydration_growth_per_minute":0.001,"hydration_action_threshold":0.25,"hunger_action_threshold":0.7,"hydration_relief":0.35,"hunger_relief":0.5,"water_utility_base":0.18,"water_utility_need_weight":1.0,"meal_utility_base":0.18,"meal_utility_need_weight":0.85,"water_staff_skill":"water_service","meal_staff_skill":"meal_service"}"#;

/// The resource service config generated Scenarios use.
pub fn default_resource_service_config() -> V {
    let parsed: serde_json::Value = serde_json::from_str(RESOURCE_SERVICE_CONFIG_JSON).expect("the constant's JSON");
    V::from_json(&parsed).expect("the constant's numbers")
}

/// `InitialResourceCondition` as the compiler reads it (warehouse `main_warehouse`, no zone).
#[derive(Clone, Debug, PartialEq)]
pub struct Stock {
    pub status: InventoryStatus,
    pub water_ml: Option<i64>,
    pub food_meal_units: Option<i64>,
    pub source_ids: Vec<String>,
    /// `"operator"` as entered, `"assumption"` from the standard provisioning.
    pub source_kind: &'static str,
}

/// `ResourceQuantityCondition` of kind `resource_delivery` into `main_warehouse`.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    /// `"water_ml"` or `"food_meal_units"`.
    pub resource_id: &'static str,
    pub quantity: i64,
    pub due_minute: i64,
    pub source_id: String,
    pub source_kind: &'static str,
    pub repeat_every_minutes: Option<i64>,
}

impl Delivery {
    /// `model_dump(mode="json")` in `OperationalConditions.scheduled`.
    pub fn dump(&self) -> V {
        map(vec![
            ("kind", text("resource_delivery")),
            ("resource_id", text(self.resource_id)),
            ("quantity", V::Int(self.quantity)),
            ("due_minute", V::Int(self.due_minute)),
            ("warehouse_id", text(DEFAULT_WAREHOUSE_ID)),
            ("source_id", text(&self.source_id)),
            ("source_kind", text(self.source_kind)),
            ("repeat_every_minutes", self.repeat_every_minutes.map_or(V::Null, V::Int)),
        ])
    }
}

/// `OperationalConditions` with only an initial inventory and deliveries.
#[derive(Clone, Debug, PartialEq)]
pub struct Conditions {
    pub initial_resources: Stock,
    pub scheduled: Vec<Delivery>,
}

/// `InitialStockSpec`.
#[derive(Clone, Debug, PartialEq)]
pub struct InitialStock {
    pub water_ml: i64,
    pub food_meal_units: i64,
    pub source_ids: Vec<String>,
}

impl InitialStock {
    pub fn warehouse_id(&self) -> &'static str {
        DEFAULT_WAREHOUSE_ID
    }

    pub fn dump(&self) -> V {
        map(vec![
            ("schema_version", V::Int(1)),
            ("warehouse_id", text(DEFAULT_WAREHOUSE_ID)),
            ("zone_id", V::Null),
            ("water_ml", V::Int(self.water_ml)),
            ("food_meal_units", V::Int(self.food_meal_units)),
            ("source_ids", texts(&self.source_ids)),
        ])
    }
}

/// What `OperationalConditions.compile` returns.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub initial_stock: Option<InitialStock>,
    /// `PlannedEventSpec.model_dump(mode="json")` of each event, in order.
    pub scheduled_events: Vec<V>,
    /// `resource_service` when the stock is entered and both services exist.
    pub model_configs: Vec<(String, V)>,
    pub blockers: Vec<String>,
}

/// A facility as the compiler reads it.
#[derive(Clone, Copy, Debug)]
pub struct FacilityKind<'a> {
    pub id: &'a str,
    pub kind: &'a str,
}

impl Conditions {
    /// The request's conditions as entered.
    pub fn of_request(conditions: &OperationalConditions) -> Self {
        let resources = &conditions.initial_resources;
        Self {
            initial_resources: Stock {
                status: resources.status,
                water_ml: resources.water_ml,
                food_meal_units: resources.food_meal_units,
                source_ids: resources.source_ids.clone(),
                source_kind: "operator",
            },
            scheduled: Vec::new(),
        }
    }

    /// `referenced_source_ids()`: sorted, without repeats.
    pub fn referenced_source_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.initial_resources.source_ids.clone();
        ids.extend(self.scheduled.iter().map(|item| item.source_id.clone()));
        ids.sort();
        ids.dedup();
        ids
    }

    /// `provenance_sources()`: a record per source ID, by ID.
    pub fn provenance_sources(&self) -> Vec<V> {
        let mut kinds: Vec<(String, &'static str)> = Vec::new();
        let mut set = |id: &str, kind: &'static str| match kinds.iter_mut().find(|(known, _)| known == id) {
            Some(entry) => entry.1 = kind,
            None => kinds.push((id.to_string(), kind)),
        };
        for id in &self.initial_resources.source_ids {
            set(id, self.initial_resources.source_kind);
        }
        for item in &self.scheduled {
            set(&item.source_id, item.source_kind);
        }
        kinds.sort();
        kinds
            .into_iter()
            .filter(|(_, kind)| ["operator", "assumption", "inferred"].contains(kind))
            .map(|(id, kind)| {
                map(vec![
                    ("id", text(&id)),
                    ("title", text("Operator-entered initial shelter conditions")),
                    ("source_kind", text(kind)),
                    (
                        "description",
                        text(if kind == "operator" {
                            "Scenario operator input; not an independently verified official observation."
                        } else {
                            "Scenario assumption or inference explicitly supplied as an input."
                        }),
                    ),
                    ("geographic_scope", V::Null),
                    ("observation_period", V::Null),
                ])
            })
            .collect()
    }

    /// `compile(duration_minutes=..., zone_ids=..., facilities=..., staff=...)`. The stock has no
    /// zone and there is no equipment or staff condition, so the zones and staff are not read.
    /// Err where Python raises (an initial stock above `MAX_RESOURCE_QUANTITY`).
    pub fn compile(&self, duration_minutes: i64, facilities: &[FacilityKind<'_>]) -> Result<Compiled, String> {
        let mut blockers = Vec::new();
        let stock = &self.initial_resources;
        if stock.status == InventoryStatus::Incomplete {
            blockers.push("初期在庫の水・食料の両方を明示してください。不明を0には変換していません。".to_string());
        }
        let initial_stock = if stock.status == InventoryStatus::Entered {
            let water_ml = stock.water_ml.unwrap_or(0);
            let food_meal_units = stock.food_meal_units.unwrap_or(0);
            if water_ml > MAX_RESOURCE_QUANTITY || food_meal_units > MAX_RESOURCE_QUANTITY {
                return Err("an initial stock quantity exceeds MAX_RESOURCE_QUANTITY".into());
            }
            Some(InitialStock { water_ml, food_meal_units, source_ids: stock.source_ids.clone() })
        } else {
            None
        };
        let mut events = Vec::new();
        let mut sequence = 0i64;
        for (index, condition) in self.scheduled.iter().enumerate() {
            if condition.due_minute >= duration_minutes {
                blockers.push("運営条件の開始時刻はシミュレーション期間内にしてください。".to_string());
                continue;
            }
            if initial_stock.is_none() {
                blockers.push("資源の納入・損失イベントには同じ倉庫の入力済み初期在庫が必要です。".to_string());
                continue;
            }
            let mut current = condition.due_minute;
            let mut count = 0;
            while current < duration_minutes && count < 129 {
                count += 1;
                sequence += 1;
                // `add_event`'s check of the minute never fails here: `current` is in the Run.
                events.push(map(vec![
                    ("id", text(&format!("region-condition-{sequence:03}-resource_delivery"))),
                    ("kind", text("exogenous")),
                    ("due_minute", V::Int(current)),
                    ("priority", V::Int(0)),
                    ("sequence", V::Int(sequence)),
                    ("event_type", text("resource_delivery")),
                    (
                        "payload",
                        map(vec![
                            ("warehouse_id", text(DEFAULT_WAREHOUSE_ID)),
                            ("resource_id", text(condition.resource_id)),
                            ("quantity", V::Int(condition.quantity)),
                            ("shipment_id", text(&format!("region-shipment-{sequence:03}"))),
                        ]),
                    ),
                    ("source_id", text(&condition.source_id)),
                    ("causation_id", V::Null),
                    (
                        "provenance",
                        map(vec![
                            ("input_path", text(&format!("operational_conditions.scheduled[{index}]"))),
                            ("source_type", text(condition.source_kind)),
                            ("units", text("simulation_minutes")),
                            ("geographic_scope", V::Null),
                            ("observation_period", V::Null),
                        ]),
                    ),
                ]));
                let Some(interval) = condition.repeat_every_minutes else { break };
                current += interval;
            }
        }
        let mut model_configs = Vec::new();
        if stock.status == InventoryStatus::Entered {
            let has = |kind: &str| facilities.iter().any(|item| item.kind == kind);
            if initial_stock.is_none() {
                blockers.push("資源サービス条件には入力済み初期在庫が必要です。".to_string());
            } else if !has("water_station") || !has("meal_distribution") {
                blockers.push("資源をシミュレーションで配布するには給水・食事配布設備の両方が必要です。".to_string());
            } else {
                model_configs.push(("resource_service".to_string(), default_resource_service_config()));
            }
        }
        if events.len() > 128 {
            blockers.push("繰り返しを含むスケジュールは128イベント以下にしてください。".to_string());
            events.clear();
        }
        Ok(Compiled { initial_stock, scheduled_events: events, model_configs, blockers })
    }
}

/// `with_standard_provisioning`: an unknown inventory becomes the standard stock with a recurring
/// resupply from `supply_start_day`, and the assumption's record; an entered or partial one is
/// kept as entered (no record).
pub fn with_standard_provisioning(
    conditions: &OperationalConditions,
    population: i64,
    duration_minutes: i64,
    supply_start_day: i64,
) -> (Conditions, Option<V>) {
    let entered = Conditions::of_request(conditions);
    if conditions.initial_resources.status != InventoryStatus::Unknown {
        return (entered, None);
    }
    let water_per_day = DRINKING_WATER_ML_PER_PERSON_DAY * population;
    let meals_per_day = MEAL_UNITS_PER_PERSON_DAY * population;
    let stock = Stock {
        status: InventoryStatus::Entered,
        water_ml: Some(water_per_day * STOCK_DAYS),
        food_meal_units: Some(meals_per_day * STOCK_DAYS),
        source_ids: vec![STANDARD_PROVISIONING_SOURCE_ID.into()],
        source_kind: "assumption",
    };
    let start_minute = (supply_start_day - 1) * 1440;
    let mut interval_days = 1;
    let mut deliveries = Vec::new();
    if start_minute < duration_minutes {
        let span_days = (duration_minutes - start_minute + 1439) / 1440;
        while 2 * ((span_days + interval_days - 1) / interval_days) > MAX_DEFAULT_DELIVERY_EVENTS {
            interval_days += 1;
        }
        for (resource_id, per_day) in [("water_ml", water_per_day), ("food_meal_units", meals_per_day)] {
            deliveries.push(Delivery {
                resource_id,
                quantity: per_day * interval_days,
                due_minute: start_minute,
                source_id: STANDARD_PROVISIONING_SOURCE_ID.into(),
                source_kind: "assumption",
                repeat_every_minutes: Some(interval_days * 1440),
            });
        }
    }
    let record = map(vec![
        ("id", text(STANDARD_PROVISIONING_SOURCE_ID)),
        ("subject", text("standard_provisioning")),
        (
            "value",
            map(vec![
                ("drinking_water_ml_per_person_day", V::Int(DRINKING_WATER_ML_PER_PERSON_DAY)),
                ("meal_units_per_person_day", V::Int(MEAL_UNITS_PER_PERSON_DAY)),
                ("stock_days", V::Int(STOCK_DAYS)),
                ("initial_water_ml", V::Int(water_per_day * STOCK_DAYS)),
                ("initial_food_meal_units", V::Int(meals_per_day * STOCK_DAYS)),
                ("supply_start_day", V::Int(supply_start_day)),
                ("supply_interval_days", if deliveries.is_empty() { V::Null } else { V::Int(interval_days) }),
                ("supply_scheduled", V::Bool(!deliveries.is_empty())),
            ]),
        ),
        (
            "reason",
            text(
                "No starting inventory was entered, so the national guidance amounts were assumed: 3 days of stock \
                 and outside supply from the chosen day. Not a measured shelter inventory or delivery plan.",
            ),
        ),
        ("source_id", text(STANDARD_PROVISIONING_SOURCE_ID)),
    ]);
    (Conditions { initial_resources: stock, scheduled: deliveries }, Some(record))
}
