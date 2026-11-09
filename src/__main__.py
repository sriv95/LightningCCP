import logging
import sys

from dotenv import load_dotenv

from src.bot import LightningBot
from src.config import load_config


def main() -> None:
    if "--bench" in sys.argv[1:]:  # offline benchmark: no token, no network
        from src.bench import main as bench

        return bench(sys.argv[1:])
    load_dotenv()
    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
    )
    config = load_config()
    LightningBot(config).run(config.token, log_handler=None)


if __name__ == "__main__":
    main()
