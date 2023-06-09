# jetrocli

[<img src="https://img.shields.io/badge/docs-jetro-blue"></img>](https://docs.rs/jetro)
[<img src="https://img.shields.io/badge/try-online%20repl-brightgreen"></img>](https://jetro.io)
![GitHub](https://img.shields.io/github/license/mitghi/jetrocli)

[Jetro](https://github.com/mitghi/jetro) command-line tool for transforming, querying and comparing JSON format.

# installation

On Mac, using Homebrew:

```bash
$ brew tap mitghi/jetrocli
$ brew install jetrocli
$ jetrocli
Transform, compare and query JSON format

Usage: jetrocli [OPTIONS] --query <QUERY>

Options:
  -q, --query <QUERY>  Jetro query
  -f, --file <FILE>    JSON filepath ( or pipe to stdin instead )
  -h, --help           Print help
  -V, --version        Print version
```

Please check the release for other platforms, there are several compiled binaries.

# examples

```
$ echo '{"some":"value", "keys": [1,2,4,8, {"name": "jetro", "isAwesome": true}]}' | jetrocli  --query ">/..keys/#filter('isAwesome' == true)/*/#formats('From {}', 'name') ->* 'message'"
{
  "message": "From jetro"
}
```
