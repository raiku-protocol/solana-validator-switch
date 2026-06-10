# Solana Validator Switch (SVS)

CLI tool for switching a Solana validator identity between an active and a standby node. The identity switch itself takes about a second; the full operation including verification takes 30-45 seconds. Supports Firedancer, Agave, Solana, and Jito validators, with optional automatic failover and Telegram alerts.

![Demo](assets/demo.gif)

## Installation

Build from source (requires Rust):

```bash
cargo build --release
# binary at target/release/svs, or install it onto your PATH:
cargo install --path .
```

## Usage

### Interactive Mode

```bash
svs           # Opens interactive menu
svs -c /path/to/custom/config.yaml  # Use custom config file
```

### Command Line Mode

```bash
svs status                    # Check validator status
svs status --validator 0      # Check specific validator by index
svs switch                    # Perform validator switch
svs switch --dry-run          # Preview switch without executing
svs switch --validator 1      # Switch specific validator by index
svs switch --min-idle-time 30 # Require 30s without leader slots before switching (default: 60)
svs switch --skip-leader-check # Switch immediately without waiting for a restart window
svs test-alert                # Test Telegram alert configuration
svs --config /path/to/config  # Use custom config file for any command
svs --version                 # Show version
svs --help                    # Show help
```

## Configuration

### Default Configuration
```bash
mkdir -p ~/.solana-validator-switch
cp config.example.yaml ~/.solana-validator-switch/config.yaml
nano ~/.solana-validator-switch/config.yaml
```

### Multiple Configurations
You can manage multiple validator setups using custom config files:

```bash
# Create different configs for different validator pairs
cp config.example.yaml ~/configs/mainnet-validators.yaml
cp config.example.yaml ~/configs/testnet-validators.yaml

# Use specific config
svs --config ~/configs/mainnet-validators.yaml status
svs -c ~/configs/testnet-validators.yaml switch
```

See [config.example.yaml](config.example.yaml) for the full configuration template.

### Telegram Alerts Setup (Optional)

To enable Telegram notifications:

1. **Create a Telegram Bot**:

   - Message [@BotFather](https://t.me/botfather) on Telegram
   - Send `/newbot` and follow the prompts
   - Save the bot token (looks like `123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11`)

2. **Get Your Chat ID**:

   - Add the bot to a group or start a private chat with it
   - Send a message to the bot
   - Visit `https://api.telegram.org/bot<YOUR_BOT_TOKEN>/getUpdates`
   - Find your chat ID in the response (negative for groups, positive for private chats)

3. **Configure in config.yaml**:

   ```yaml
   alert_config:
     enabled: true
     delinquency_threshold_seconds: 30 # Alert after 30 seconds without voting
     telegram:
       bot_token: "YOUR_BOT_TOKEN"
       chat_id: "YOUR_CHAT_ID"
   ```

4. **Test Your Configuration**:
   ```bash
   svs test-alert
   ```

You'll receive notifications for:

- **Validator Delinquency** (CRITICAL): When your validator stops voting for more than 30 seconds
  - Only triggers when SSH and RPC are both working (no false alarms)
- **SSH / RPC Connection Failures** (LOW PRIORITY): After 30 minutes of continuous failures
- **Switch Results**: Success/failure notifications with timing details

## Key Features

- **Fast switching**: ~1 second identity switch with streaming tower transfer
- **Automatic failover**: Switches to the standby when the active validator stops voting
- **Runtime status detection**: Auto-detects which node is active and which is standby
- **SSH connection pooling**: Persistent multiplexed connections for instant command execution
- **Universal validator support**: Works with Firedancer, Agave, Solana, and Jito
- **Interactive dashboard**: Real-time terminal UI showing both nodes
  - Active/standby status, current identity, and version for each node
  - SSH and RPC health checks with countdown timers
  - Manual switch trigger with the (S) key
  - Tab key to cycle through multiple validator pairs
- **Telegram alerts**: Delinquency, failover events, switch results, and SSH/RPC failures

## Security

- **No credential storage**: SSH private keys never leave your `~/.ssh/` directory
- **Path-only configuration**: Only file paths and hostnames stored in config files
- **No network exposure**: Tool operates through SSH connections only
- **Local execution**: All operations run locally, no external services

## License

MIT — see [LICENSE](LICENSE). Forked from [huiskylabs/solana-validator-switch](https://github.com/huiskylabs/solana-validator-switch).
