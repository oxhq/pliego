<?php

declare(strict_types=1);

namespace Pliego\Php\Exception;

use RuntimeException;

/** The executable does not advertise the exact API 2 tuple required by this SDK. */
final class UnsupportedContractException extends RuntimeException {}
