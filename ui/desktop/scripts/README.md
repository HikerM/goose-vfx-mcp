# Luminay

Put `luminay` in your $PATH if you want to launch via:

```
luminay .
```

This will open lumina GUI from any path you specify

# Unregister Deeplink Protocols (macos only)

`unregister-deeplink-protocols.js` is a script to unregister the deeplink protocol used by lumina like `lumina://`.
This is handy when you want to test deeplinks with the development version of Lumina.

# Usage

To unregister the deeplink protocols, run the following command in your terminal:
Then launch Lumina again and your deeplinks should work from the latest launched lumina application as it is registered on startup.

```bash
node scripts/unregister-deeplink-protocols.js
```

