//! ECO-12 / BUILD-08: shipped component, real containers, native movement and receipts.
//! Seeded conservation fixtures are intentionally separate from ordinary source proof.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use mc_plugin_host::{DeploymentConfig, DiscoveryMode, HostQueues, PluginLimits, discover, start_deployment};
use mc_protocol::packets::Packet;
use mc_protocol::packets::play::{AddEntity, ServerboundChatCommand, ServerboundAttack, ServerboundMovePlayerPos, MovePlayerFlags, MoveEntityPos, MoveEntityPosRot, EntityPositionSync, ClientboundSetEntityData, ClientboundTakeItemEntity, EntityDataValue, ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundKeepAlive, ServerboundKeepAlive, SynchronizePlayerPosition, ConfirmTeleportation};
use mc_server::wire_client::Client;
#[path="support/settlement.rs"]
mod support;
use support::*;
const PACKAGE:&str="solaris-settlements";
const SOURCE:&str="routefrom";
const TARGET:&str="routeto";
fn data_stack(slot: &mc_world::FurnaceSlot) -> mc_data::ItemStack {
    if slot.is_empty() { mc_data::ItemStack::EMPTY } else {
        mc_data::ItemStack { count: slot.count, item_id: slot.item_id, damage: slot.damage, enchantments: slot.enchantments.clone(), custom_name: slot.custom_name.as_deref().map(Into::into), item_model: slot.item_model.clone(), stew_effects: slot.stew_effects.clone(), components: slot.components.clone() }
    }
}
fn chest_slot(stack: &mc_data::ItemStack) -> mc_world::FurnaceSlot {
    if stack.is_empty() { mc_world::FurnaceSlot::EMPTY } else {
        mc_world::FurnaceSlot { count: stack.count, item_id: stack.item_id, damage: stack.damage, enchantments: stack.enchantments.clone(), custom_name: stack.custom_name.as_deref().map(str::to_owned), item_model: stack.item_model.clone(), stew_effects: stack.stew_effects.clone(), components: stack.components.clone() }
    }
}
struct LiveServer { address:std::net::SocketAddr, shutdown:mc_net::ShutdownHandle, task:tokio::task::JoinHandle<std::io::Result<()>>, host:mc_plugin_host::PluginHost }
impl LiveServer { async fn stop(self){self.shutdown.request();tokio::time::timeout(Duration::from_secs(10),self.task).await.expect("shutdown timeout").expect("server task").expect("server result");tokio::task::spawn_blocking(move||self.host.stop()).await.expect("component stop");} }
fn deployment(root:&Path)->DeploymentConfig {DeploymentConfig{root:root.to_owned(),mode:DiscoveryMode::Strict,expected:vec![PACKAGE.into()],grants:BTreeMap::from([(PACKAGE.into(),vec!["storage","storage_batches","player_queries","inventory_transfers","persistent_npcs","npc_work","npc_orders","world_structures"].into_iter().map(str::to_owned).collect())]),require_grants:true,precommit_hooks:Vec::new()}}
async fn start_server(deployment:&DeploymentConfig,vanilla:&Path,world:Arc<tokio::sync::Mutex<mc_world::WorldStorage>>,blocks:Arc<mc_world::BlockRegistry>,items:Arc<mc_data::items::ItemRegistry>)->LiveServer {
    let data=Arc::new(mc_data::load(vanilla).expect("vanilla data"));let tags=Arc::new(mc_data::tags::load(vanilla,&data).expect("vanilla tags"));let report=mc_data::blocks::load_blocks_report(vanilla.join("reports/blocks.json")).expect("block report");
    let sessions=mc_net::PlayerSessionsHandle::new();let limits=PluginLimits::default();
    let host=start_deployment(discover(deployment,&limits).expect("strict discovery").into_packages(),limits,HostQueues::default(),Arc::new(ServerSessions(sessions.clone()))).expect("shipped component");
    assert!(host.contribution().refusal().is_none(),"real contributions must be admitted");
    let item_facts=mc_data::item_components::solaris_required_item_facts().with_custom_items(host.contribution().items().map(|(_,i)|i.clone()),&items).expect("item catalog").with_work_recipes(host.contribution().work_recipes().map(|(_,r)|r.clone()),&items).expect("recipe catalog");
    let shutdown=mc_net::ShutdownHandle::default();
    let cfg=mc_net::ServerConfig{tab_list:mc_net::TabListConfig::default(),bind_address:"127.0.0.1:0".parse().unwrap(),motd:"physical caravan acceptance".into(),max_players:2,view_distance:10,data,blocks,world:Some(world),tags,recipes:Arc::new(mc_data::recipes::solaris_required_recipes()),loot:Arc::new(mc_data::loot::builtin().clone()),block_light:None,items,item_facts:Arc::new(item_facts),block_facts:Arc::new(mc_data::block_facts::BlockFactsTable::from_blocks_report(&report)),entity_types:Arc::new(mc_data::entity_types::solaris_required_entity_types()),biome_spawns:Arc::new(mc_data::biomes::solaris_required_biome_spawn_rules()),chunk_pipeline:mc_net::ChunkPipelinePolicy::default(),random_tick:mc_simulation::tick_policy::RandomTickPolicy::default(),command_permissions:mc_net::CommandPermissionConfig::new([PLAYER],false),loader_manifest:None,shutdown:shutdown.clone()};
    let bound=mc_net::bind_with_scripts(cfg,host.boundary().clone()).await.expect("scripted server");bound.register_player_sessions(&sessions);let address=bound.local_addr().expect("address");let task=tokio::spawn(async move{bound.serve().await});LiveServer{address,shutdown,task,host}
}
struct Fixture {
    _plugins:tempfile::TempDir, root:tempfile::TempDir, deployment:DeploymentConfig, vanilla:std::path::PathBuf,
    world:Arc<tokio::sync::Mutex<mc_world::WorldStorage>>,blocks:Arc<mc_world::BlockRegistry>,items:Arc<mc_data::items::ItemRegistry>,
    generator:Arc<mc_worldgen::TerrainGenerator>, chunks:((i32,i32),(i32,i32)), source:mc_world::BlockPos,target:mc_world::BlockPos,stance:mc_world::BlockPos,resident:String,route:String,
}
async fn fixture(seeded:u32,foreign_destination:bool)->(Fixture,LiveServer,Client) {
    let plugins=tempfile::tempdir().expect("deployment root");copy_settlement_package(plugins.path());let deployment=deployment(plugins.path());let vanilla=core_root().join("data/vanilla");
    let data=Arc::new(mc_data::load(&vanilla).expect("data"));let tags=Arc::new(mc_data::tags::load(&vanilla,&data).expect("tags"));
    let blocks=Arc::new(mc_world::BlockRegistry::from_report(&mc_data::blocks::load_blocks_report(vanilla.join("reports/blocks.json")).expect("blocks")).expect("registry"));
    let items=Arc::new(mc_data::items::ItemRegistry::from_report(&mc_data::items::load_items_report(vanilla.join("reports/registries.json")).expect("items")));
    let root=tempfile::tempdir().expect("world root");std::fs::create_dir_all(root.path().join("region")).expect("regions");
    let (physical,_,start,chunks,_,generator)=materialized_vanilla_village_world(root.path(),&vanilla,&blocks,&items,&data,&tags);
    let world=Arc::new(tokio::sync::Mutex::new(physical));let server=start_server(&deployment,&vanilla,Arc::clone(&world),Arc::clone(&blocks),Arc::clone(&items)).await;let (mut client,source_owner)=connect_play_named_with_uuid(server.address,PLAYER).await;
    command_and_wait(&mut client,&format!("settlement create {SOURCE} small"),&format!("Founded {SOURCE}")).await;
    let mut recipient=if foreign_destination{Some(connect_play_named(server.address,"RouteBuyer").await)}else{None};
    if let Some(recipient)=recipient.as_mut(){
        command_and_wait(recipient,&format!("settlement create {TARGET} small"),&format!("Founded {TARGET}")).await;
        command_and_wait(recipient,&format!("settlement role {TARGET} {source_owner} steward"),"Roles updated").await;
    }else{command_and_wait(&mut client,&format!("settlement create {TARGET} small"),&format!("Founded {TARGET}")).await;}
    let sites=command_and_wait_any(&mut client,&format!("settlement site {SOURCE}")," sites: ").await;let prefix=format!("village_{}_{}_",start.0,start.1);
    let feature=sites.split(": ").nth(1).into_iter().flat_map(|s|s.split(',')).map(str::trim).map(|s|s.split('@').next().unwrap_or_default()).find(|s|s.starts_with(&prefix)).expect("materialized vanilla village").to_owned();
    // Observe original generated villagers while the genuine source footprint streams.
    // Only the observer travels; neither the actor nor its UUID/pose is synthesized.
    client.write_packet(&ServerboundChatCommand{command:format!("tp {} 250 {}",start.0*16,start.1*16)}).await.expect("observer travel");
    let originals=observe_generated_residents(&mut client,chunks.0,chunks.1,1).await;
    command_and_wait(&mut client,&format!("settlement adopt {SOURCE} {feature}"),"Adopted site").await;
    let (original_uuid,original_pose)=originals.first_key_value().expect("actual original native villager");
    command_and_wait(&mut client,&format!("tp {} {} {}",original_pose[0],original_pose[1],original_pose[2]),"Teleported to").await;
    command_and_wait(&mut client,&format!("settlement attach {SOURCE} {original_uuid}"),&format!("Attached resident_1 in {SOURCE}")).await;
    let resident=resident_handle(&command_and_wait_any(&mut client,&format!("settlement info {SOURCE}"),"residents=").await);
    let carrier=command_and_wait_any(&mut client,&format!("settlement caravan-carrier {SOURCE} resident_1"),"Caravan carrier ").await;
    let carrier=native(&carrier);let pose=&carrier["pose"];assert_eq!(pose["dimension"],"minecraft:overworld");
    assert_eq!(carrier["uuid"].as_str(),Some(original_uuid.as_str()),"travel uses the original observed native person, never a replacement");
    let stance=mc_world::BlockPos{x:pose["x"].as_f64().expect("actual carrier x").floor() as i32,y:pose["y"].as_f64().expect("actual carrier y").floor() as i32,z:pose["z"].as_f64().expect("actual carrier z").floor() as i32};
    let source=mc_world::BlockPos{z:stance.z+1,..stance};let target=mc_world::BlockPos{x:stance.x+24,z:stance.z+1,..stance};
    command_and_wait(&mut client,&format!("tp {} {} {}",stance.x,stance.y,stance.z-2),"Teleported to").await;
    let air=block_state(&blocks,"minecraft:air",&[]);let stone=block_state(&blocks,"minecraft:stone",&[]);let chest=block_state(&blocks,"minecraft:chest",&[]);
    {let mut w=world.lock().await;for x in stance.x-3..=stance.x+27 {for z in stance.z-3..=stance.z+4 {for y in stance.y..=stance.y+2 {w.set_block_at(mc_world::BlockPos{x,y,z},air).expect("clear physical yard").expect("loaded yard");}w.set_block_at(mc_world::BlockPos{x,y:stance.y-1,z},stone).expect("walkable physical floor").expect("loaded floor");}}
        for at in [source,target] {w.set_block_at(at,chest).expect("physical chest").expect("loaded chest");w.set_chest_block_entity(at,mc_world::ChestBlockEntity::default()).expect("empty chest");}
        if seeded>0 {
            let mut stock=mc_world::ChestBlockEntity::default();
            let oak=items.id_of(&mc_data::Identifier::parse("minecraft:oak_log").unwrap()).unwrap();
            stock.slots[0]=chest_slot(&mc_data::item_stack::ItemStack::new(oak,seeded as i32)
                .with_custom_name("CP053 conserved cargo")
                .with_custom_data(&[10,0,0,3,0,5,b'c',b'p',b'0',b'5',b'3',0,0,0,1,0]).expect("canonical fixture custom data"));
            w.set_chest_block_entity(source,stock).expect("explicit conservation fixture only");
        }
    }
    for (scope,container) in [(SOURCE,source),(TARGET,target)] {
        let entrance=mc_world::BlockPos{x:container.x-2,z:container.z,..container};
        for (half,y) in [("lower",entrance.y),("upper",entrance.y+1)] {let door=block_state(&blocks,"minecraft:oak_door",&[("facing","north"),("half",half),("hinge","left"),("open","true"),("powered","false")]);world.lock().await.set_block_at(mc_world::BlockPos{y,..entrance},door).expect("real entrance").expect("loaded entrance");}
        command_and_wait(&mut client,&format!("tp {} {} {}",container.x,container.y,container.z-2),"Teleported to").await;
        command_and_wait(&mut client,&format!("settlement survey {scope} plot"),"plot surveyed.").await;
        command_and_wait(&mut client,&format!("settlement recognize {scope} stock solaris:warehouse entrance:{},{},{} storage:{},{},{}",entrance.x,entrance.y,entrance.z,container.x,container.y,container.z),"Certificate ").await;
    }
    if let Some(recipient)=recipient.as_mut(){command_and_wait(recipient,&format!("settlement role {TARGET} {source_owner} member"),"Roles updated").await;}
    drop(recipient);
    let route=format!("{},{},{};{},{},{};{},{},{}",stance.x,stance.y,stance.z,stance.x+12,stance.y,stance.z,stance.x+24,stance.y,stance.z);
    (Fixture{_plugins:plugins,root,deployment,vanilla,world,blocks,items,generator,chunks,source,target,stance,resident,route},server,client)
}
async fn count(f:&Fixture,at:mc_world::BlockPos,item:&str)->i32 {chest_item_count(&f.world.lock().await.chest_block_entity(at).expect("physical query").expect("physical chest"),&f.items,item)}
async fn status(client:&mut Client,id:&str)->String {command_and_wait_any(client,&format!("settlement caravan-status {id}"),"Caravan ").await}
async fn until(client:&mut Client,id:&str,predicate:impl Fn(&str)->bool)->String {tokio::time::timeout(Duration::from_secs(90),async{loop{let line=status(client,id).await;if predicate(&line){return line;}}}).await.expect("actual native caravan transition")}
fn native(line:&str)->serde_json::Value {serde_json::from_str(line.split_once(" native=").expect("native query evidence").1.split(" cargo=").next().unwrap()).expect("actual native status JSON")}
fn cargo_count(line:&str,item:&str)->u64 {let cargo:serde_json::Value=serde_json::from_str(line.split_once(" cargo=").expect("actual cargo query").1.split(" cargo-fence=").next().unwrap()).expect("actual cargo JSON");cargo.as_array().unwrap().iter().filter(|s|s["stack"]["resource"]==item).map(|s|s["count"].as_u64().unwrap()).sum()}
async fn caravan_command(client:&mut Client,command:&str,expected:&str)->String {
    tokio::time::timeout(Duration::from_secs(30),async {
        loop {
            let line=command_and_wait_any(client,command,"Caravan ").await;
            // Busy is an explicit no-effect refusal; reissue only that same player action.
            if line.starts_with("Caravan busy;"){continue;}
            assert!(line.contains(expected),"actual caravan command refusal: {line}");
            return line;
        }
    }).await.expect("bounded real caravan command response")
}
async fn create(client:&mut Client,f:&Fixture,id:&str,item:&str,quantity:u32){caravan_command(client,&format!("settlement caravan {id} {SOURCE} stock {TARGET} stock resident_1 {item} {quantity} 5 2000 {}",f.route),&format!("Caravan {id} committed:")).await;}
async fn stop_work(client:&mut Client,resident:&str)->u64 {
    command_and_wait_any(client,&format!("settlement stop-work {SOURCE} resident_1"),"Stop-work ").await;
    tokio::time::timeout(Duration::from_secs(30),async{loop{
        let work=command_and_wait_any(client,&format!("settlement work-status {resident}"),"Work ").await;
        if work.contains("state=Cancelled"){return work.split_once(" revision=").and_then(|(_,r)|r.split_whitespace().next()).and_then(|r|r.parse().ok()).expect("actual cancellation revision");}
    }}).await.expect("native work cancellation before travel")
}
async fn bridge(f:&Fixture,intact:bool){let state=block_state(&f.blocks,if intact{"minecraft:stone"}else{"minecraft:air"},&[]);let mut w=f.world.lock().await;for x in f.stance.x+10..=f.stance.x+14{for z in f.stance.z-3..=f.stance.z+4{w.set_block_at(mc_world::BlockPos{x,y:f.stance.y-1,z},state).expect("real bridge change").expect("loaded bridge");}}}
#[tokio::test]
#[ignore="explicit ECO12/BUILD08 shipped-component gate; populated vanilla sidecars required"]
async fn eco12_build08_physical_route_conserves_blocked_full_restart_replay(){
    let (mut f,server,mut client)=fixture(3,false).await;
    let original=f.world.lock().await.chest_block_entity(f.source).unwrap().unwrap().slots[0].clone();
    // Admission refusal changes neither source nor destination, and never creates a carrier inventory.
    bridge(&f,false).await;create(&mut client,&f,"blocked","minecraft:oak_log",3).await;
    let blocked=until(&mut client,"blocked",|s|s.contains("status=Blocked")&&!s.contains("Caravan busy")).await;
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,3);assert_eq!(count(&f,f.target,"minecraft:oak_log").await,0);assert!(blocked.contains("native refusal")||blocked.contains("Blocked"));
    bridge(&f,true).await;
    let loaded=until(&mut client,"blocked",|s|s.contains("status=Transit")&&s.contains("loaded=true")&&s.contains(" cargo=")&&native(s)["state"]=="Running").await;assert_eq!(cargo_count(&loaded,"minecraft:oak_log"),3);assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);
    let start=native(&loaded);let start_x=start["pose"]["x"].as_f64().expect("actual starting pose");
    let epoch=start["epoch"].as_u64().unwrap();caravan_command(&mut client,&format!("settlement caravan-park blocked {epoch}"),"Caravan blocked committed:").await;
    let parked=until(&mut client,"blocked",|s|s.contains("parked=true")&&s.contains(" cargo=")).await;assert_eq!(cargo_count(&parked,"minecraft:oak_log"),3);let parked_native=native(&parked);assert!(parked_native["epoch"].as_u64().unwrap()>epoch);assert!(parked_native["handoff"]["snapshot"].as_str().is_some());
    let parked_fence:serde_json::Value=serde_json::from_str(parked.split_once(" cargo-fence=").unwrap().1).unwrap();
    assert_eq!(parked_native["handoff"]["inventory_hash"],parked_fence["hash"],"handoff freezes the same actual canonical carry image");
    assert!(parked_native["handoff"]["reservation_refs"].as_array().unwrap().iter().any(|r|Some(r.as_str().unwrap())==parked_native["reservation_ref"].as_str()));
    caravan_command(&mut client,&format!("settlement caravan-resume blocked {epoch}"),"current native executor epoch required").await;
    bridge(&f,false).await;let parked_epoch=parked_native["epoch"].as_u64().unwrap();caravan_command(&mut client,&format!("settlement caravan-resume blocked {parked_epoch}"),"Caravan blocked ").await;
    let blocked=until(&mut client,"blocked",|s|s.contains("status=Blocked")&&s.contains(" cargo=")).await;assert_eq!(cargo_count(&blocked,"minecraft:oak_log"),3);assert_eq!(count(&f,f.source,"minecraft:oak_log").await+count(&f,f.target,"minecraft:oak_log").await+3,3);
    let mut full=mc_world::ChestBlockEntity::default();for slot in &mut full.slots{slot.item_id=f.items.id_of(&mc_data::Identifier::parse("minecraft:cobblestone").unwrap()).unwrap();slot.count=64;}f.world.lock().await.set_chest_block_entity(f.target,full).expect("actual full destination");
    drop(client);server.stop().await;f.world.lock().await.flush_dirty().expect("persist native world");
    let cold=mc_world::WorldStorage::open_with_capacity(f.root.path(),Arc::clone(&f.blocks),2048).expect("cold world").with_item_registry(Arc::clone(&f.items)).with_generator(Arc::clone(&f.generator) as Arc<dyn mc_world::ChunkGenerator>).with_spawn(mc_world::WorldSpawn::new(f.stance.x+4096,f.stance.z+4096));f.world=Arc::new(tokio::sync::Mutex::new(cold));
    let restarted=start_server(&f.deployment,&f.vanilla,Arc::clone(&f.world),Arc::clone(&f.blocks),Arc::clone(&f.items)).await;let mut client=connect_play(restarted.address).await;
    let cold=until(&mut client,"blocked",|s|s.contains("Unloaded")||s.contains("unloaded")).await;assert!(!cold.contains("status=Completed"),"unloaded prerequisites are not delivery");
    command_and_wait(&mut client,&format!("tp {} {} {}",f.stance.x,f.stance.y,f.stance.z-2),"Teleported to").await;wait_for_chunks(&mut client,f.chunks.0,f.chunks.1).await;bridge(&f,true).await;
    let warm=until(&mut client,"blocked",|s|s.contains(" cargo=")&&native(s)["lifecycle"]=="AliveLoaded").await;
    if native(&warm)["mode"]=="Parked" {
        let epoch=native(&warm)["epoch"].as_u64().unwrap();
        caravan_command(&mut client,&format!("settlement caravan-resume blocked {epoch}"),"Caravan blocked committed:").await;
    }
    let full=until(&mut client,"blocked",|s|s.contains("destination full")&&s.contains(" cargo=")).await;assert_eq!(cargo_count(&full,"minecraft:oak_log"),3);assert!(native(&full)["arrival"]["receipt"].as_str().is_some());assert!(native(&full)["pose"]["x"].as_f64().unwrap()>start_x,"the same physical carrier actually traversed the route");
    f.world.lock().await.set_chest_block_entity(f.target,mc_world::ChestBlockEntity::default()).expect("make actual capacity");
    let delivered=until(&mut client,"blocked",|s|s.contains("status=Completed")&&s.contains(" cargo=")).await;assert_eq!(cargo_count(&delivered,"minecraft:oak_log"),0);assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);assert_eq!(count(&f,f.target,"minecraft:oak_log").await,3);
    assert_eq!(f.world.lock().await.chest_block_entity(f.target).unwrap().unwrap().slots[0],original,"all authorized stack components survive physical delivery");
    assert!(native(&delivered)["native_ticks"].as_u64().unwrap()<=2000,"finite admitted native cost budget is not reset by restart/handoff");
    assert!(native(&delivered)["geometry_cells"].as_u64().unwrap()<=3*128,"native geometry admission remains bounded");
    let receipts=native(&delivered)["receipts"].clone();assert_eq!(receipts.as_array().unwrap().iter().filter(|r|r["effect"]=="unload").count(),1);
    caravan_command(&mut client,&format!("settlement caravan blocked {SOURCE} stock {TARGET} stock resident_1 minecraft:oak_log 3 5 2000 {}",f.route),"already recorded; no cargo withdrawn twice").await;
    assert_eq!(count(&f,f.target,"minecraft:oak_log").await,3);assert_eq!(native(&status(&mut client,"blocked").await)["receipts"],receipts);
    drop(client);restarted.stop().await;
}
#[tokio::test]
#[ignore="explicit native carrier attack/loss/loot gate; populated vanilla sidecars required"]
async fn eco12_native_attack_loses_carrier_cargo_without_source_refund_or_delivery(){
    let (f,server,mut client)=fixture(3,false).await;
    let original=f.world.lock().await.chest_block_entity(f.source).unwrap().unwrap().slots[0].clone();
    create(&mut client,&f,"loss","minecraft:oak_log",3).await;
    let loaded=until(&mut client,"loss",|s|s.contains("status=Transit")&&s.contains("loaded=true")&&s.contains(" cargo=")&&native(s)["state"]=="Running").await;let witness=native(&loaded);let epoch=witness["epoch"].as_u64().unwrap();
    caravan_command(&mut client,&format!("settlement caravan-park loss {epoch}"),"Caravan loss committed:").await;let parked=until(&mut client,"loss",|s|s.contains("parked=true")&&s.contains(" cargo=")).await;let proof=native(&parked);let entity=proof["entity_id"].as_i64().expect("actual native entity id") as i32;let pose=&proof["pose"];
    command_and_wait(&mut client,&format!("tp {} {} {}",pose["x"].as_f64().unwrap()+1.0,pose["y"].as_f64().unwrap(),pose["z"].as_f64().unwrap()),"Teleported to").await;
    // Only real protocol attacks produce damage/death. No test writes HP or invents a loot position.
    let oak=f.items.id_of(&mc_data::Identifier::parse("minecraft:oak_log").unwrap()).unwrap();
    let item_type=mc_data::entity_types::solaris_required_entity_types().id_of(&mc_data::Identifier::parse("minecraft:item").unwrap()).and_then(|id|i32::try_from(id).ok()).expect("native item entity type");
    let cargo:serde_json::Value=serde_json::from_str(parked.split_once(" cargo=").unwrap().1.split(" cargo-fence=").next().unwrap()).unwrap();
    let parcel=cargo.as_array().unwrap().iter().find(|s|s["stack"]["resource"]=="minecraft:oak_log").expect("actual admitted native parcel").clone();
    let loot=tokio::time::timeout(Duration::from_secs(90),async{
        let mut stacks=BTreeMap::new();let mut picked=BTreeMap::new();let mut spawns=BTreeMap::new();let mut inventory=BTreeMap::new();
        let mut actual=[pose["x"].as_f64().unwrap(),pose["y"].as_f64().unwrap(),pose["z"].as_f64().unwrap()];
        loop{
            client.write_packet(&ServerboundMovePlayerPos{x:actual[0]+1.0,y:actual[1],z:actual[2],flags:MovePlayerFlags::new(true,false)}).await.expect("real observer follows native motion");
            client.write_packet(&ServerboundAttack{entity_id:entity}).await.expect("real client attack");
            for _ in 0..8 {
                let mut frame=client.read_frame().await.expect("native attack frame");
                if frame.id==AddEntity::ID {let spawn=AddEntity::decode(&mut frame.body).expect("real physical loot spawn");if spawn.entity_type_id==item_type{spawns.insert(spawn.entity_id,spawn.uuid.to_string());}}
                else if frame.id==MoveEntityPos::ID {let m=MoveEntityPos::decode(&mut frame.body).expect("native move");if m.entity_id==entity{actual[0]+=f64::from(m.delta_x)/4096.0;actual[1]+=f64::from(m.delta_y)/4096.0;actual[2]+=f64::from(m.delta_z)/4096.0;}}
                else if frame.id==MoveEntityPosRot::ID {let m=MoveEntityPosRot::decode(&mut frame.body).expect("native move/rotation");if m.entity_id==entity{actual[0]+=f64::from(m.delta_x)/4096.0;actual[1]+=f64::from(m.delta_y)/4096.0;actual[2]+=f64::from(m.delta_z)/4096.0;}}
                else if frame.id==EntityPositionSync::ID {let m=EntityPositionSync::decode(&mut frame.body).expect("native pose sync");if m.entity_id==entity{actual=[m.values.position.x,m.values.position.y,m.values.position.z];}}
                else if frame.id==ClientboundSetEntityData::ID {
                    let metadata=ClientboundSetEntityData::decode(&mut frame.body).expect("native item metadata");
                    for value in metadata.values {if let EntityDataValue::ItemStack{stack,..}=value {if spawns.contains_key(&metadata.entity_id)&&stack.item_id==oak&&stack.count==3{stacks.insert(metadata.entity_id,stack);}}}
                } else if frame.id==ClientboundTakeItemEntity::ID {let take=ClientboundTakeItemEntity::decode(&mut frame.body).expect("native pickup");if stacks.contains_key(&take.item_entity_id){*picked.entry(take.item_entity_id).or_insert(0)+=take.amount;}}
                else if frame.id==ClientboundContainerSetSlot::ID {let slot=ClientboundContainerSetSlot::decode(&mut frame.body).expect("canonical pickup inventory slot");if slot.container_id==0||slot.container_id== -2{inventory.insert(slot.slot,slot.item_stack);}}
                else if frame.id==ClientboundContainerSetContent::ID {let content=ClientboundContainerSetContent::decode(&mut frame.body).expect("canonical pickup inventory image");if content.container_id==0{inventory=content.items.into_iter().enumerate().map(|(slot,stack)|(i16::try_from(slot).unwrap(),stack)).collect();}}
                else if frame.id==ClientboundKeepAlive::ID {let alive=ClientboundKeepAlive::decode(&mut frame.body).expect("native keepalive");client.write_packet(&ServerboundKeepAlive{id:alive.id}).await.expect("actual observer keepalive");}
                else if frame.id==SynchronizePlayerPosition::ID {let sync=SynchronizePlayerPosition::decode(&mut frame.body).expect("actual observer correction");client.write_packet(&ConfirmTeleportation{teleport_id:sync.teleport_id}).await.expect("ack actual observer correction");}
                if picked.values().sum::<i32>()>=3&&inventory.values().filter(|s|chest_slot(s)==original).map(|s|s.count).sum::<i32>()>=3{return(stacks,picked,spawns,inventory);}
            }
        }
    }).await.expect("actual exact oak-log death drop and pickup, not ledger disappearance");
    assert_eq!(loot.1.values().sum::<i32>(),3,"actual canonical cargo reaches matching native loot once");
    assert_eq!(loot.0.values().map(|s|s.count).sum::<i32>(),3,"matching item metadata preserves exact stack count");
    assert!(loot.0.values().all(|s|chest_slot(s)==original),"native death loot preserves the exact source stack components");
    let lost=until(&mut client,"loss",|s|s.contains("status=Lost")&&s.contains(" native=")&&native(s)["lifecycle"]=="Dead").await;assert_eq!(native(&lost)["state"],"Dead");assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);assert_eq!(count(&f,f.target,"minecraft:oak_log").await,0);
    let loss=native(&lost);let death=&loss["death_cargo"];
    assert_eq!(loss["death_ref"],death["receipt"],"durable loss cites the canonical native cargo-death decision");
    assert_eq!(death["uuid"],proof["uuid"]);assert_eq!(death["epoch"],proof["epoch"]);
    assert_eq!(death["loot"].as_array().unwrap().iter().filter(|l|l["inventory"]=="Carry"&&l["slot"]==parcel["slot"]&&l["stack"]==parcel["stack"]).map(|l|l["count"].as_u64().unwrap()).sum::<u64>(),3);
    for (entity_id,stack) in &loot.0 {
        let uuid=&loot.2[entity_id];
        assert!(death["loot"].as_array().unwrap().iter().any(|l|l["uuid"].as_str()==Some(uuid.as_str())&&l["count"].as_i64()==Some(i64::from(stack.count))&&l["stack"]==parcel["stack"]),"native death receipt refers to the independently observed real item UUID and full components");
    }
    assert_eq!(loot.3.values().filter(|s|chest_slot(s)==original).map(|s|s.count).sum::<i32>(),3,"real pickup credits the canonical player inventory exactly once");
    drop(client);server.stop().await;
}
#[tokio::test]
#[ignore="ordinary generated crop source proof, separate from seeded conservation fixture"]
async fn eco12_ordinary_source_then_physical_transport(){
    let (mut f,server,mut client)=fixture(0,false).await;
    assert_eq!(count(&f,f.source,"minecraft:wheat").await,0,"no seeded commodity source");
    let crop={let mut w=f.world.lock().await;let mut found=None;for x in f.chunks.0.0*16..=f.chunks.1.0*16+15{for z in f.chunks.0.1*16..=f.chunks.1.1*16+15{for y in f.stance.y-16..=f.stance.y+16{let at=mc_world::BlockPos{x,y,z};if let Some(state)=w.get_block(at).expect("generated source read").and_then(|id|f.blocks.by_id(id)){if state.block.id.as_str()=="minecraft:wheat"&&state.properties.iter().any(|(k,v)|k=="age"&&v=="7"){found=Some(at);break;}}}if found.is_some(){break;}}if found.is_some(){break;}}found.expect("seed supplies an actual mature generated village wheat source outside the manual route")};
    // The only seeded helper is a tool; crop coordinates/state come from generation.
    let mut tools=mc_world::ChestBlockEntity::default();tools.slots[0].item_id=f.items.id_of(&mc_data::Identifier::parse("minecraft:wooden_hoe").unwrap()).unwrap();tools.slots[0].count=1;f.world.lock().await.set_chest_block_entity(f.source,tools).expect("tool fixture, no goods");
    command_and_wait(&mut client,&format!("tp {} {} {}",f.source.x,f.source.y,f.source.z-2),"Teleported to").await;
    command_and_wait(&mut client,&format!("settlement survey {SOURCE} plot"),"plot surveyed.").await;
    command_and_wait(&mut client,&format!("settlement bind-manual {} {} {}",f.source.x,f.source.y,f.source.z),"Warehouse bound.").await;
    command_and_wait_any(&mut client,&format!("settlement issue {} 0 equipment minecraft:wooden_hoe",f.resident),"Haul assigned. Revision ").await;
    let revision=stop_work(&mut client,&f.resident).await;
    command_and_wait_any(&mut client,&format!("settlement harvest {} {revision} minecraft:wooden_hoe {} {} {} {} {} {}",f.resident,crop.x,crop.y,crop.z,crop.x,crop.y,crop.z),"Harvest assigned. Revision ").await;
    tokio::time::timeout(Duration::from_secs(90),async{loop{let state=f.world.lock().await.get_block(crop).expect("native source query").expect("loaded generated crop");if state!=block_state(&f.blocks,"minecraft:wheat",&[("age","7")]){break;}tokio::task::yield_now().await;}}).await.expect("actual generated crop harvested");
    let revision=stop_work(&mut client,&f.resident).await;
    command_and_wait(&mut client,&format!("settlement haul {} {revision} minecraft:wheat",f.resident),"Haul assigned. Revision ").await;
    let amount=tokio::time::timeout(Duration::from_secs(90),async{loop{let n=count(&f,f.source,"minecraft:wheat").await;if n>0{return n;}tokio::task::yield_now().await;}}).await.expect("actual native harvest+haul");
    let state=f.world.lock().await.get_block(crop).expect("physical crop after harvest").expect("loaded crop");assert_ne!(state,block_state(&f.blocks,"minecraft:wheat",&[("age","7")]),"ordinary work changed the actual generated source");
    stop_work(&mut client,&f.resident).await;
    let position=command_and_wait_any(&mut client,&format!("settlement caravan-carrier {SOURCE} resident_1"),"Caravan carrier ").await;let position=native(&position);let position=&position["pose"];
    let actual=mc_world::BlockPos{x:position["x"].as_f64().unwrap().floor() as i32,y:position["y"].as_f64().unwrap().floor() as i32,z:position["z"].as_f64().unwrap().floor() as i32};
    assert_eq!((actual.x-f.source.x).abs()+(actual.z-f.source.z).abs(),1,"native haul ended at an actual source-container stance");assert_eq!(actual.y,f.source.y);
    let mut points=vec![actual];
    if actual.z>f.source.z&&actual.x==f.source.x{points.push(mc_world::BlockPos{x:f.source.x+2,..actual});points.push(mc_world::BlockPos{x:f.source.x+2,z:f.stance.z,..actual});}
    else if actual.z!=f.stance.z{points.push(mc_world::BlockPos{z:f.stance.z,..actual});}
    points.push(mc_world::BlockPos{x:f.stance.x+12,..f.stance});points.push(mc_world::BlockPos{x:f.stance.x+24,..f.stance});
    f.route=points.iter().map(|p|format!("{},{},{}",p.x,p.y,p.z)).collect::<Vec<_>>().join(";");
    create(&mut client,&f,"ordinary","minecraft:wheat",amount as u32).await;
    let delivered=until(&mut client,"ordinary",|s|s.contains("status=Completed")&&s.contains(" cargo=")).await;assert_eq!(cargo_count(&delivered,"minecraft:wheat"),0);assert_eq!(count(&f,f.source,"minecraft:wheat").await,0);assert_eq!(count(&f,f.target,"minecraft:wheat").await,amount);
    drop(client);server.stop().await;
}

#[tokio::test]
#[ignore="explicit destination-rights-at-native-arrival gate; populated vanilla sidecars required"]
async fn eco12_destination_rights_are_checked_at_arrival_not_route_admission(){
    let (f,server,mut client)=fixture(3,true).await;
    create(&mut client,&f,"rights","minecraft:oak_log",3).await;
    let arrived=until(&mut client,"rights",|s|s.contains("current source/recipient rights")&&s.contains(" cargo=")&&native(s)["state"]=="Arrived").await;
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0,"authorized source was physically withdrawn");
    assert_eq!(count(&f,f.target,"minecraft:oak_log").await,0,"foreign recipient was not bypassed by route admission");
    assert_eq!(cargo_count(&arrived,"minecraft:oak_log"),3,"unaccepted goods stay in the real carrier");
    assert!(native(&arrived)["arrival"]["receipt"].as_str().is_some(),"refusal happens at genuine native arrival");
    drop(client);server.stop().await;
}

async fn cold_restart(f:&mut Fixture,server:LiveServer,client:Client)->(LiveServer,Client) {
    drop(client);
    server.stop().await;
    f.world.lock().await.flush_dirty().expect("persist real native decisions");
    let cold=mc_world::WorldStorage::open_with_capacity(f.root.path(),Arc::clone(&f.blocks),2048).expect("cold world")
        .with_item_registry(Arc::clone(&f.items))
        .with_generator(Arc::clone(&f.generator) as Arc<dyn mc_world::ChunkGenerator>)
        .with_spawn(mc_world::WorldSpawn::new(f.stance.x+4096,f.stance.z+4096));
    f.world=Arc::new(tokio::sync::Mutex::new(cold));
    let server=start_server(&f.deployment,&f.vanilla,Arc::clone(&f.world),Arc::clone(&f.blocks),Arc::clone(&f.items)).await;
    let mut client=connect_play(server.address).await;
    command_and_wait(&mut client,&format!("tp {} {} {}",f.stance.x,f.stance.y,f.stance.z-2),"Teleported to").await;
    wait_for_chunks(&mut client,f.chunks.0,f.chunks.1).await;
    (server,client)
}

#[tokio::test]
#[ignore="real source-debit and destination-credit cold-restart gate; populated vanilla sidecars required"]
async fn eco12_restart_at_native_inventory_effects_preserves_one_original_parcel() {
    let (mut f,server,mut client)=fixture(3,false).await;
    let original=f.world.lock().await.chest_block_entity(f.source).unwrap().unwrap().slots[0].clone();
    let mut full=mc_world::ChestBlockEntity::default();
    for slot in &mut full.slots {
        slot.item_id=f.items.id_of(&mc_data::Identifier::parse("minecraft:cobblestone").unwrap()).unwrap();
        slot.count=64;
    }
    f.world.lock().await.set_chest_block_entity(f.target,full).expect("real recipient full");
    create(&mut client,&f,"effects","minecraft:oak_log",3).await;
    // Trigger from the committed physical inventory, not a guessed plugin phase or delay.
    // This does not claim an unobserved missing-ACK window.
    tokio::time::timeout(Duration::from_secs(90),async {
        loop {
            if count(&f,f.source,"minecraft:oak_log").await==0 {break;}
            tokio::task::yield_now().await;
        }
    }).await.expect("real source debit before first cold restart");
    let (server,mut client)=cold_restart(&mut f,server,client).await;
    let arrived=until(&mut client,"effects",|s|s.contains("destination full")&&s.contains(" cargo=")&&native(s)["state"]=="Arrived").await;
    assert_eq!(cargo_count(&arrived,"minecraft:oak_log"),3);
    let arrival=native(&arrived);
    let load=arrival["receipts"].as_array().unwrap().iter().find(|r|r["effect"]=="load").expect("original native escrow transfer recovered").clone();
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);
    f.world.lock().await.set_chest_block_entity(f.target,mc_world::ChestBlockEntity::default()).expect("real recipient capacity");
    tokio::time::timeout(Duration::from_secs(90),async {
        loop {
            if count(&f,f.target,"minecraft:oak_log").await==3 {break;}
            tokio::task::yield_now().await;
        }
    }).await.expect("real destination credit before second cold restart");
    let (server,mut client)=cold_restart(&mut f,server,client).await;
    let completed=until(&mut client,"effects",|s|s.contains("status=Completed")&&s.contains(" cargo=")).await;
    let final_native=native(&completed);
    assert_eq!(final_native["route"],arrival["route"],"restart never admits a replacement cargo executor");
    assert_eq!(final_native["uuid"],arrival["uuid"]);
    assert_eq!(cargo_count(&completed,"minecraft:oak_log"),0);
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);
    assert_eq!(count(&f,f.target,"minecraft:oak_log").await,3);
    assert_eq!(f.world.lock().await.chest_block_entity(f.target).unwrap().unwrap().slots[0],original);
    let receipts=final_native["receipts"].as_array().unwrap();
    let mut loads=receipts.iter().filter(|r|r["effect"]=="load");
    assert_eq!(loads.next(),Some(&load));
    assert!(loads.next().is_none(),"one original load receipt survives both physical-effect restarts");
    assert_eq!(receipts.iter().filter(|r|r["effect"]=="unload").count(),1);
    let repeated=status(&mut client,"effects").await;
    assert_eq!(native(&repeated)["receipts"],final_native["receipts"]);
    assert_eq!(count(&f,f.target,"minecraft:oak_log").await,3);
    drop(client);server.stop().await;
}

#[tokio::test]
#[ignore="real fenced partial-stack unload and cold-restart gate; populated vanilla sidecars required"]
async fn eco12_partial_matching_destination_capacity_unloads_atomically_once() {
    let (mut f,server,mut client)=fixture(3,false).await;
    let original=f.world.lock().await.chest_block_entity(f.source).unwrap().unwrap().slots[0].clone();
    let facts=mc_data::item_components::solaris_required_item_facts();
    let maximum=mc_data::item_semantics_26_1_2::max_stack_for_stack(&facts,&f.items,&data_stack(&original));
    assert!(maximum>3,"real fixture component stack supports two partial receiving slots");
    let cobble=mc_data::item_stack::ItemStack::new(f.items.id_of(&mc_data::Identifier::parse("minecraft:cobblestone").unwrap()).unwrap(),1);
    let cobble_maximum=mc_data::item_semantics_26_1_2::max_stack_for_stack(&facts,&f.items,&cobble);
    let mut recipient=mc_world::ChestBlockEntity::default();
    for slot in &mut recipient.slots {
        *slot=chest_slot(&cobble);
        slot.count=cobble_maximum;
    }
    recipient.slots[0]=chest_slot(&data_stack(&original).with_custom_name("Different physical escrow"));
    recipient.slots[0].count=1; // Same resource, ample apparent space, incompatible components.
    recipient.slots[1]=original.clone();recipient.slots[1].count=maximum-1;
    recipient.slots[2]=original.clone();recipient.slots[2].count=maximum-2;
    let before=recipient.clone();
    f.world.lock().await.set_chest_block_entity(f.target,recipient).expect("actual fragmented receiving inventory");
    create(&mut client,&f,"partial","minecraft:oak_log",3).await;
    let completed=until(&mut client,"partial",|s|s.contains("status=Completed")&&s.contains(" cargo=")).await;
    let completed_native=native(&completed);
    assert_eq!(completed_native["state"],"Cancelled","receiver ownership is committed before executor retirement");
    assert_eq!(cargo_count(&completed,"minecraft:oak_log"),0);
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);
    let actual=f.world.lock().await.chest_block_entity(f.target).unwrap().unwrap();
    let mut expected=before;
    expected.slots[1].count=maximum;expected.slots[2].count=maximum;
    assert_eq!(actual.slots,expected.slots,"exact native room deposits two portions while preserving mismatched/full slots and every component");
    let receipts=completed_native["receipts"].as_array().unwrap();
    assert_eq!(receipts.iter().filter(|r|r["effect"]=="load").count(),1);
    assert_eq!(receipts.iter().filter(|r|r["effect"]=="unload").count(),1,"two receiving portions share one durable atomic native operation");
    let (server,mut client)=cold_restart(&mut f,server,client).await;
    let reopened=until(&mut client,"partial",|s|s.contains("status=Completed")&&s.contains(" cargo=")).await;
    assert_eq!(native(&reopened)["receipts"],completed_native["receipts"]);
    assert_eq!(cargo_count(&reopened,"minecraft:oak_log"),0);
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,0);
    assert_eq!(f.world.lock().await.chest_block_entity(f.target).unwrap().unwrap().slots,expected.slots,"restart and repeated status cannot credit either receiving portion again");
    drop(client);server.stop().await;
}

#[tokio::test]
#[ignore="real paid-caravan no-escrow refusal gate; populated vanilla sidecars required"]
async fn eco12_paid_caravan_without_escrow_refuses_source_withdrawal() {
    let (f,server,mut client)=fixture(3,false).await;
    let original=f.world.lock().await.chest_block_entity(f.source).unwrap().unwrap().slots[0].clone();
    let carrier=command_and_wait_any(&mut client,&format!("settlement caravan-carrier {SOURCE} resident_1"),"Caravan carrier ").await;
    let carrier_uuid=native(&carrier)["uuid"].clone();
    caravan_command(&mut client,&format!("settlement paid-caravan unpaid {SOURCE} stock {TARGET} stock resident_1 minecraft:oak_log 3 5 2000 {}",f.route),"Caravan unpaid committed:").await;
    let blocked=until(&mut client,"unpaid",|s|s.contains("paid mandate exact escrow/components/rights revision not ready")).await;
    assert!(blocked.contains("loaded=false"));
    assert_eq!(native(&blocked)["uuid"],carrier_uuid,"escrow refusal never replaces the actual carrier");
    assert!(native(&blocked)["route"].is_null(),"no route admitted without the original physical escrow");
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,3);
    assert_eq!(count(&f,f.target,"minecraft:oak_log").await,0);
    assert_eq!(f.world.lock().await.chest_block_entity(f.source).unwrap().unwrap().slots[0],original);
    let repeated=status(&mut client,"unpaid").await;
    assert!(repeated.contains("loaded=false"));
    assert_eq!(count(&f,f.source,"minecraft:oak_log").await,3,"repeated blocked polling cannot withdraw an unpaid parcel");
    drop(client);server.stop().await;
}
