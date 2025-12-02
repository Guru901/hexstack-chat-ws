use lume::database::Database;
use lume::database::error::DatabaseError;
use lume::define_schema;
use lume::row::Row;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use wynd::wynd::Wynd;

// Shared state to store user names
type UserNames = Arc<RwLock<HashMap<String, String>>>;

define_schema! {
    ChatMessage {
        text: String,
        sender: String,
        timestamp: String,
    }
}

#[derive(Serialize)]
struct Message {
    message_type: MessageType,
    data: String,
}

#[derive(Serialize)]
enum MessageType {
    System,
    Welcome,
    PastMessages,
    Chat,
}

#[tokio::main]
async fn main() {
    // This simplified server version runs the chat server directly, no CLI.
    let mut wynd: Wynd<TcpStream> = Wynd::new();
    let user_names: UserNames = Arc::new(RwLock::new(HashMap::new()));

    create_tables().await.unwrap();

    wynd.on_connection(move |conn| {
        let user_names = user_names.clone();
        async move {
            conn.on_open(move |handle| {
                async move {
                    let room = "main";
                    if let Err(e) = handle.join(room).await {
                        eprintln!("Failed to join room: {}", e);
                        return;
                    }

                    let messages = get_messages().await.unwrap();

                    for message in messages {
                        let message = Message {
                            message_type: MessageType::PastMessages,
                            data: format!(
                                "{}: {}",
                                message.get(ChatMessage::sender()).unwrap(),
                                message.get(ChatMessage::text()).unwrap().to_string()
                            ),
                        };
                        if let Err(e) = handle
                            .send_text(serde_json::to_string(&message).unwrap())
                            .await
                        {
                            eprintln!("Failed to send message: {}", e);
                        }
                    }

                    // Ask for the user's name
                    let message = Message {
                        message_type: MessageType::Welcome,
                        data: "Welcome! Please enter your name:".to_string(),
                    };
                    if let Err(e) = handle
                        .send_text(serde_json::to_string(&message).unwrap())
                        .await
                    {
                        eprintln!("Failed to send name prompt: {}", e);
                    }
                }
            })
            .await;

            // Handle incoming messages
            let user_names1 = user_names.clone();
            conn.on_text(move |event, handle| {
                let user_names = user_names.clone();
                async move {
                    let room = "main";
                    let user_id = handle.id().to_string();

                    // Check if user has set their name
                    let has_name = {
                        let names = user_names.read().await;
                        names.contains_key(&user_id)
                    };

                    if !has_name {
                        // First message is their name
                        let name = event.data.trim().to_string();
                        if name.is_empty() {
                            let message = Message {
                                message_type: MessageType::System,
                                data: "Name cannot be empty. Please enter your name:".to_string(),
                            };
                            if let Err(e) = handle
                                .send_text(serde_json::to_string(&message).unwrap())
                                .await
                            {
                                eprintln!("Failed to send message: {}", e);
                            }
                            return;
                        }

                        // Store the name
                        {
                            let mut names = user_names.write().await;
                            names.insert(user_id.clone(), name.clone());
                        }

                        // Send welcome message
                        let message = Message {
                            message_type: MessageType::Welcome,
                            data: format!("Welcome, {}! You can start chatting now.", name),
                        };
                        if let Err(e) = handle
                            .send_text(serde_json::to_string(&message).unwrap())
                            .await
                        {
                            eprintln!("Failed to send message: {}", e);
                        }

                        // Announce to others
                        if let Err(e) = handle
                            .to(room)
                            .text(format!("{} joined the chat!", name))
                            .await
                        {
                            eprintln!("Failed to broadcast join: {}", e);
                        }
                    } else {
                        // Regular chat message - broadcast with their name
                        let name = {
                            let names = user_names.read().await;
                            names
                                .get(&user_id)
                                .cloned()
                                .unwrap_or_else(|| user_id.clone())
                        };

                        save_message(&event.data, &name).await.unwrap();

                        let message = Message {
                            message_type: MessageType::Chat,
                            data: format!("{}: {}", name, event.data),
                        };

                        // Send to others with their name
                        if let Err(e) = handle
                            .to(room)
                            .text(serde_json::to_string(&message).unwrap())
                            .await
                        {
                            eprintln!("Failed to broadcast message: {}", e);
                        }

                        // Echo back to sender with "Me:"
                        let message = Message {
                            message_type: MessageType::Chat,
                            data: format!("Me: {}", event.data),
                        };
                        if let Err(e) = handle
                            .send_text(serde_json::to_string(&message).unwrap())
                            .await
                        {
                            eprintln!("Failed to echo message: {}", e);
                        }
                    }
                }
            });

            // Support binary messages (broadcast to all in room except sender)
            let user_names = user_names1.clone();
            conn.on_binary(move |event, handle| {
                let user_names = user_names.clone();
                async move {
                    let room = "main";
                    let user_id = handle.id().to_string();

                    let name = {
                        let names = user_names.read().await;
                        names
                            .get(&user_id)
                            .cloned()
                            .unwrap_or_else(|| user_id.clone())
                    };

                    // Broadcast binary data with user identification
                    if let Err(e) = handle
                        .to(room)
                        .emit_text(format!(
                            "{} sent binary data ({} bytes)",
                            name,
                            event.data.len()
                        ))
                        .await
                    {
                        eprintln!("Failed to broadcast binary message: {}", e);
                    }
                }
            });

            // Clean up when user disconnects
            conn.on_close(|_| async move {});
        }
    });

    wynd.listen(3000, || {
        println!("Chat server listening on port 3000");
    })
    .await
    .unwrap();
}

async fn save_message(text: &str, sender: &str) -> Result<(), DatabaseError> {
    let db = Database::connect("sqlite://chat.sqlite").await?;

    let message = ChatMessage {
        text: text.to_string(),
        sender: sender.to_string(),
        timestamp: chrono::Utc::now().to_string(),
    };

    db.insert(message).execute().await?;

    Ok(())
}

async fn get_messages() -> Result<Vec<Row<ChatMessage>>, DatabaseError> {
    let db = Database::connect("sqlite://chat.sqlite").await?;

    let messages = db
        .query::<ChatMessage, SelectChatMessage>()
        .execute()
        .await?;

    Ok(messages)
}

async fn create_tables() -> Result<(), DatabaseError> {
    let db = Database::connect("sqlite://chat.sqlite").await?;
    db.register_table::<ChatMessage>().await?;
    Ok(())
}
