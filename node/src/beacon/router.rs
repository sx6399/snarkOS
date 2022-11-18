// Copyright (C) 2019-2022 Aleo Systems Inc.
// This file is part of the snarkOS library.

// The snarkOS library is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The snarkOS library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with the snarkOS library. If not, see <https://www.gnu.org/licenses/>.

use super::*;

use snarkos_node_messages::{DisconnectReason, Message, MessageCodec};
use snarkos_node_tcp::{
    protocols::{Disconnect, Handshake, Writing},
    Connection,
    ConnectionSide,
    Tcp,
};
use snarkvm::prelude::Network;

use core::time::Duration;
use rand::Rng;
use snarkos_node_router::Routes;
use snarkos_node_tcp::{protocols::Reading, P2P};
use std::{io, net::SocketAddr, sync::atomic::Ordering, time::Instant};

impl<N: Network> P2P for Beacon<N> {
    /// Returns a reference to the TCP instance.
    fn tcp(&self) -> &Tcp {
        &self.router.tcp()
    }
}

#[async_trait]
impl<N: Network> Handshake for Beacon<N> {
    /// Performs the handshake protocol.
    async fn perform_handshake(&self, mut connection: Connection) -> io::Result<Connection> {
        let peer_addr = connection.addr();
        let conn_side = connection.side();
        let stream = self.borrow_stream(&mut connection);
        self.router.handshake(peer_addr, stream, conn_side).await?;

        Ok(connection)
    }
}

#[async_trait]
impl<N: Network> Disconnect for Beacon<N> {
    /// Any extra operations to be performed during a disconnect.
    async fn handle_disconnect(&self, peer_addr: SocketAddr) {
        self.router.remove_connected_peer(peer_addr);
    }
}

#[async_trait]
impl<N: Network> Writing for Beacon<N> {
    type Codec = MessageCodec<N>;
    type Message = Message<N>;

    /// Creates an [`Encoder`] used to write the outbound messages to the target stream.
    /// The `side` parameter indicates the connection side **from the node's perspective**.
    fn codec(&self, _addr: SocketAddr, _side: ConnectionSide) -> Self::Codec {
        Default::default()
    }
}

#[async_trait]
impl<N: Network> Reading for Beacon<N> {
    type Codec = MessageCodec<N>;
    type Message = Message<N>;

    /// Creates a [`Decoder`] used to interpret messages from the network.
    /// The `side` param indicates the connection side **from the node's perspective**.
    fn codec(&self, _peer_addr: SocketAddr, _side: ConnectionSide) -> Self::Codec {
        Default::default()
    }

    /// Processes a message received from the network.
    async fn process_message(&self, peer_ip: SocketAddr, message: Self::Message) -> io::Result<()> {
        // Update the timestamp for the received message.
        self.router().connected_peers.read().get(&peer_ip).map(|peer| {
            peer.insert_seen_message(message.id(), rand::thread_rng().gen());
        });

        // Process the message.
        let success = self.handle_message(peer_ip, message).await;

        // Disconnect if the peer violated the protocol.
        if !success {
            warn!("Disconnecting from '{peer_ip}' (violated protocol)");
            self.send(peer_ip, Message::Disconnect(DisconnectReason::ProtocolViolation.into()));
            // Disconnect from this peer.
            let _disconnected = self.tcp().disconnect(peer_ip).await;
            debug_assert!(_disconnected);
            // Restrict this peer to prevent reconnection.
            self.router().insert_restricted_peer(peer_ip);
        }

        Ok(())
    }
}

#[async_trait]
impl<N: Network> Routes<N> for Beacon<N> {
    /// The maximum number of peers permitted to maintain connections with.
    const MAXIMUM_NUMBER_OF_PEERS: usize = 10;

    fn router(&self) -> &Router<N> {
        &self.router
    }

    /// Retrieves the latest epoch challenge and latest block, and returns the puzzle response to the peer.
    async fn puzzle_request(&self, peer_ip: SocketAddr) -> bool {
        // Retrieve the latest epoch challenge and latest block.
        let (epoch_challenge, block) = {
            // Retrieve the latest epoch challenge.
            let epoch_challenge = match self.ledger.latest_epoch_challenge() {
                Ok(block) => block,
                Err(error) => {
                    error!("Failed to retrieve latest epoch challenge for a puzzle request: {error}");
                    return false;
                }
            };
            // Retrieve the latest block.
            let block = self.ledger.latest_block();

            // Scope drops the read lock on the consensus module.
            (epoch_challenge, block)
        };
        // Send the `PuzzleResponse` message to the peer.
        self.send(peer_ip, Message::PuzzleResponse(PuzzleResponse { epoch_challenge, block: Data::Object(block) }));
        true
    }

    /// Adds the unconfirmed solution to the memory pool, and propagates the solution to all peers.
    async fn unconfirmed_solution(
        &self,
        _peer_ip: SocketAddr,
        _message: UnconfirmedSolution<N>,
        solution: ProverSolution<N>,
    ) -> bool {
        // Add the unconfirmed solution to the memory pool.
        if let Err(error) = self.consensus.add_unconfirmed_solution(&solution) {
            trace!("[UnconfirmedSolution] {error}");
            return true; // Maintain the connection.
        }
        // // Propagate the `UnconfirmedSolution` to connected beacons.
        // let request = RouterRequest::MessagePropagateBeacon(Message::UnconfirmedSolution(message), vec![peer_ip]);
        // if let Err(error) = router.process(request).await {
        //     warn!("[UnconfirmedSolution] {error}");
        // }
        true
    }

    /// Adds the unconfirmed transaction to the memory pool, and propagates the transaction to all peers.
    fn unconfirmed_transaction(
        &self,
        _peer_ip: SocketAddr,
        _message: UnconfirmedTransaction<N>,
        transaction: Transaction<N>,
    ) -> bool {
        // Add the unconfirmed transaction to the memory pool.
        if let Err(error) = self.consensus.add_unconfirmed_transaction(transaction) {
            trace!("[UnconfirmedTransaction] {error}");
            return true; // Maintain the connection.
        }
        // // Propagate the `UnconfirmedTransaction`.
        // let request = RouterRequest::MessagePropagate(Message::UnconfirmedTransaction(message), vec![peer_ip]);
        // if let Err(error) = router.process(request).await {
        //     warn!("[UnconfirmedTransaction] {error}");
        // }
        true
    }
}
