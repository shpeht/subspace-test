use futures::stream::StreamExt;
use std::path::PathBuf;

use bytesize::ByteSize;
use clap::Parser;
use sc_network_common::config::MultiaddrWithPeerId;
use subspace_sdk::{
    chain_spec, farmer::CacheDescription, Farmer, Node, PlotDescription, PublicKey,
};
use tempfile::TempDir;

#[derive(clap::Parser, Debug)]
enum Args {
    Farm {
        /// Path to the plot
        #[arg(short, long)]
        plot: PathBuf,

        /// Size of the plot
        #[arg(long)]
        plot_size: ByteSize,

        /// Path to the node directory
        #[arg(short, long)]
        node: PathBuf,

        /// Path to the chain spec
        #[arg(short, long)]
        spec: PathBuf,
    },
    Sync {
        /// Bootstrap nodes
        #[arg(short, long)]
        boot_nodes: Vec<MultiaddrWithPeerId>,

        /// Path to the chain spec
        #[arg(short, long)]
        spec: PathBuf,
    },
    GenerateSpec {
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let args = Args::parse();
    match args {
        Args::GenerateSpec { path } => {
            tokio::fs::write(
                path,
                serde_json::to_string_pretty(
                    &chain_spec::dev_config()
                        .map_err(|err| anyhow::anyhow!("Failed to generate a chain spec: {err}"))?,
                )?,
            )
            .await?
        }
        Args::Farm {
            plot,
            plot_size,
            node,
            spec,
        } => {
            let chain_spec = serde_json::from_str(&tokio::fs::read_to_string(spec).await?)?;
            let (plot_size, cache_size) = (
                ByteSize::b(plot_size.as_u64() * 9 / 10),
                ByteSize::b(plot_size.as_u64() / 10),
            );
            let node = Node::builder()
                .listen_on(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()])
                .force_authoring(true)
                .force_synced(true)
                .role(subspace_sdk::node::Role::Authority)
                .build(node, chain_spec)
                .await?;

            let plots = [PlotDescription::new(plot.join("plot"), plot_size)];
            let _farmer: Farmer = Farmer::builder()
                .build(
                    PublicKey::from([13; 32]),
                    node.clone(),
                    &plots,
                    CacheDescription::new(plot.join("cache"), cache_size)?,
                )
                .await?;

            let addr = node.listen_addresses().await?.into_iter().next().unwrap();
            tracing::info!(%addr, "Node listening at");

            node.subscribe_new_blocks()
                .await?
                .for_each(|block| async move { tracing::info!(?block, "New block!") })
                .await;
        }
        Args::Sync { boot_nodes, spec } => {
            let node = TempDir::new()?;
            let chain_spec = serde_json::from_str(&tokio::fs::read_to_string(spec).await?)?;
            let node = Node::builder()
                .force_authoring(true)
                .role(subspace_sdk::node::Role::Authority)
                .boot_nodes(boot_nodes)
                .build(node.as_ref(), chain_spec)
                .await?;

            node.sync().await;
            tracing::info!("Node was synced!");

            node.subscribe_new_blocks()
                .await?
                .for_each(|block| async move { tracing::info!(?block, "New block!") })
                .await;
        }
    }

    Ok(())
}
