# Twitcher

**this application is made for a small event with some friends :)**


# Build/Compile
to compile the application:
```bash
cargo build

# for release mode (more performance)
cargo build --release
```

now to run the application you can either:
- use cargo
```bash
cargo run -- --token <your-oauth-token> --channel <channel-name-to-join>

# for release mode (more performance)
cargo run --release -- --token <your-oauth-token> --channel <channel-name-to-join>
```

# Usage
the application is supposed to have the basic vim bindings

h j k l for left down up right

yy to yank a message

dd to delete your message

$ to go to the end of the line
^ to go to the beginning of the line

# Known issues

the application doesn't support typing in other than ASCII characters due to the way cursor movements are handled
