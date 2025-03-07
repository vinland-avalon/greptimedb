// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use api::v1::{ColumnDataType, ColumnDef, CreateTableExpr, TableId};
use client::{Client, Database};
use prost_09::Message;
use substrait_proto::protobuf::plan_rel::RelType as PlanRelType;
use substrait_proto::protobuf::read_rel::{NamedTable, ReadType};
use substrait_proto::protobuf::rel::RelType;
use substrait_proto::protobuf::{PlanRel, ReadRel, Rel};
use tracing::{event, Level};

fn main() {
    tracing::subscriber::set_global_default(tracing_subscriber::FmtSubscriber::builder().finish())
        .unwrap();

    run();
}

#[tokio::main]
async fn run() {
    let client = Client::with_urls(vec!["127.0.0.1:3001"]);

    let create_table_expr = CreateTableExpr {
        catalog_name: "greptime".to_string(),
        schema_name: "public".to_string(),
        table_name: "test_logical_dist_exec".to_string(),
        desc: "".to_string(),
        column_defs: vec![
            ColumnDef {
                name: "timestamp".to_string(),
                datatype: ColumnDataType::TimestampMillisecond as i32,
                is_nullable: false,
                default_constraint: vec![],
            },
            ColumnDef {
                name: "key".to_string(),
                datatype: ColumnDataType::Uint64 as i32,
                is_nullable: false,
                default_constraint: vec![],
            },
            ColumnDef {
                name: "value".to_string(),
                datatype: ColumnDataType::Uint64 as i32,
                is_nullable: false,
                default_constraint: vec![],
            },
        ],
        time_index: "timestamp".to_string(),
        primary_keys: vec!["key".to_string()],
        create_if_not_exists: false,
        table_options: Default::default(),
        table_id: Some(TableId { id: 1024 }),
        region_ids: vec![0],
    };

    let db = Database::new("create table", client.clone());
    let result = db.create(create_table_expr).await.unwrap();
    event!(Level::INFO, "create table result: {:#?}", result);

    let logical = mock_logical_plan();
    event!(Level::INFO, "plan size: {:#?}", logical.len());
    let db = Database::new("greptime", client);
    let result = db.logical_plan(logical).await.unwrap();

    event!(Level::INFO, "result: {:#?}", result);
}

fn mock_logical_plan() -> Vec<u8> {
    let catalog_name = "greptime".to_string();
    let schema_name = "public".to_string();
    let table_name = "test_logical_dist_exec".to_string();

    let named_table = NamedTable {
        names: vec![catalog_name, schema_name, table_name],
        advanced_extension: None,
    };
    let read_type = ReadType::NamedTable(named_table);

    let read_rel = ReadRel {
        common: None,
        base_schema: None,
        filter: None,
        projection: None,
        advanced_extension: None,
        read_type: Some(read_type),
    };

    let mut buf = vec![];
    let rel = Rel {
        rel_type: Some(RelType::Read(Box::new(read_rel))),
    };
    let plan_rel = PlanRel {
        rel_type: Some(PlanRelType::Rel(rel)),
    };
    plan_rel.encode(&mut buf).unwrap();

    buf
}
