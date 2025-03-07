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

mod stream;

use std::pin::Pin;
use std::sync::Arc;

use api::v1::GreptimeRequest;
use arrow_flight::flight_service_server::FlightService;
use arrow_flight::{
    Action, ActionType, Criteria, Empty, FlightData, FlightDescriptor, FlightInfo,
    HandshakeRequest, HandshakeResponse, PutResult, SchemaResult, Ticket,
};
use async_trait::async_trait;
use common_grpc::flight::{FlightEncoder, FlightMessage};
use common_query::Output;
use common_runtime::Runtime;
use futures::Stream;
use prost::Message;
use snafu::ResultExt;
use tokio::sync::oneshot;
use tonic::{Request, Response, Status, Streaming};

use crate::error;
use crate::grpc::flight::stream::FlightRecordBatchStream;
use crate::query_handler::grpc::ServerGrpcQueryHandlerRef;

type TonicResult<T> = Result<T, Status>;
type TonicStream<T> = Pin<Box<dyn Stream<Item = TonicResult<T>> + Send + Sync + 'static>>;

pub(crate) struct FlightHandler {
    handler: ServerGrpcQueryHandlerRef,
    runtime: Arc<Runtime>,
}

impl FlightHandler {
    pub(crate) fn new(handler: ServerGrpcQueryHandlerRef, runtime: Arc<Runtime>) -> Self {
        Self { handler, runtime }
    }
}

#[async_trait]
impl FlightService for FlightHandler {
    type HandshakeStream = TonicStream<HandshakeResponse>;

    async fn handshake(
        &self,
        _: Request<Streaming<HandshakeRequest>>,
    ) -> TonicResult<Response<Self::HandshakeStream>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    type ListFlightsStream = TonicStream<FlightInfo>;

    async fn list_flights(
        &self,
        _: Request<Criteria>,
    ) -> TonicResult<Response<Self::ListFlightsStream>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    async fn get_flight_info(
        &self,
        _: Request<FlightDescriptor>,
    ) -> TonicResult<Response<FlightInfo>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    async fn get_schema(
        &self,
        _: Request<FlightDescriptor>,
    ) -> TonicResult<Response<SchemaResult>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    type DoGetStream = TonicStream<FlightData>;

    async fn do_get(&self, request: Request<Ticket>) -> TonicResult<Response<Self::DoGetStream>> {
        let ticket = request.into_inner().ticket;
        let request =
            GreptimeRequest::decode(ticket.as_slice()).context(error::InvalidFlightTicketSnafu)?;

        let (tx, rx) = oneshot::channel();
        let handler = self.handler.clone();

        // Executes requests in another runtime to
        // 1. prevent the execution from being cancelled unexpected by Tonic runtime;
        // 2. avoid the handler blocks the gRPC runtime incidentally.
        self.runtime.spawn(async move {
            let result = handler.do_query(request).await;

            // Ignore the sending result.
            // Usually an error indicates the rx at Tonic side is dropped (due to request timeout).
            let _ = tx.send(result);
        });

        // Safety: An early-dropped tx usually indicates a serious problem (like panic).
        // This unwrap is used to poison the upper layer.
        let output = rx.await.unwrap()?;

        let stream = to_flight_data_stream(output);
        Ok(Response::new(stream))
    }

    type DoPutStream = TonicStream<PutResult>;

    async fn do_put(
        &self,
        _: Request<Streaming<FlightData>>,
    ) -> TonicResult<Response<Self::DoPutStream>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    type DoExchangeStream = TonicStream<FlightData>;

    async fn do_exchange(
        &self,
        _: Request<Streaming<FlightData>>,
    ) -> TonicResult<Response<Self::DoExchangeStream>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    type DoActionStream = TonicStream<arrow_flight::Result>;

    async fn do_action(&self, _: Request<Action>) -> TonicResult<Response<Self::DoActionStream>> {
        Err(Status::unimplemented("Not yet implemented"))
    }

    type ListActionsStream = TonicStream<ActionType>;

    async fn list_actions(
        &self,
        _: Request<Empty>,
    ) -> TonicResult<Response<Self::ListActionsStream>> {
        Err(Status::unimplemented("Not yet implemented"))
    }
}

fn to_flight_data_stream(output: Output) -> TonicStream<FlightData> {
    match output {
        Output::Stream(stream) => {
            let stream = FlightRecordBatchStream::new(stream);
            Box::pin(stream) as _
        }
        Output::RecordBatches(x) => {
            let stream = FlightRecordBatchStream::new(x.as_stream());
            Box::pin(stream) as _
        }
        Output::AffectedRows(rows) => {
            let stream = tokio_stream::once(Ok(
                FlightEncoder::default().encode(FlightMessage::AffectedRows(rows))
            ));
            Box::pin(stream) as _
        }
    }
}
