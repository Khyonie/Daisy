# Daisy

Self-contained RAG chat app for PDF and Markdown documents, with query decomposition, multi-query rewriting, dedupe, and reranking.

<img width="850" alt="image" src="https://github.com/user-attachments/assets/47e30337-9330-473c-b39d-24dd570be38f" />

# Quickstart
### Docker 🐳
```shell
docker pull ghcr.io/khyonie/documentllm:latest

docker run -d -p 3001:3001 ghcr.io/khyonie/documentllm:latest
```
> [!NOTE]
> This will automatically fetch `gemma4:e4b`, so it can be rapidly installed on remote machines with no setup.

Connect to the website at `127.0.0.1:3001`.

### Running from source 🖥️
> [!IMPORTANT]
> Ensure `cargo`, `npm`, and `ollama` are all installed.

Clone the repository:
```shell
git clone https://github.com/Khyonie/DocumentLLM.git

cd DocumentLLM
```
Build the frontend:
```shell
npm install

npm run build --prefix docllm-frontend/
```
Then, start the backend:
```
cargo run
```

Connect to the website at `127.0.0.1:3001`.

# Program Flow

<img width="540" height="942" alt="llmflow (1)" src="https://github.com/user-attachments/assets/e647d916-ff16-460b-9ee8-0f6c287e2668" />

